//! Cursor (`cursor`) usage provider:
//! - Embedded `rusqlite` (`bundled`) credential discovery supporting:
//!   1. Non-locking read-only SQLite query against `state.vscdb` (`file:...?mode=ro&immutable=1` + `0600` temp-copy
//!      WAL fallback), extracting `cursorAuth/accessToken`, `cursorAuth/cachedEmail`, and
//!      `cursorAuth/stripeMembershipType` with zero dependency on external `/usr/bin/sqlite3`.
//!   2. Electron `safeStorage` `v10` (`peanuts`) and Linux `v11` (`org.freedesktop.secrets` when unlocked)
//!      in-memory decryption, degrading cleanly with account email + plan badge preserved if keyring is locked.
//!   3. Manual JSON or plain-text token files (`{"accessToken": "..."}` / `{"cursorAuth/accessToken": "..."}`).
//!   4. `CURSOR_ACCESS_TOKEN` environment variable.
//! - Connect-RPC usage fetch (`POST https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage`),
//!   emitting `Cursor Models`, `Other Models`, and `31d` buckets plus `Ultra` / `Pro` plan badges.

use crate::cache::CachedAccountState;
use crate::config::{abbreviate_home, expand_tilde_with_home, ManualAccountConfig, PollMode};
use crate::http::decode_jwt_payload;
use crate::model::{AccountReport, AccountState, BucketLayout, CreditSummary, UsageBucket};
use crate::proc_exec::{run_command, CommandSpec};
use crate::providers::{
    AccountDescriptor, AccountFetchOutcome, FetchContext, ProviderMeta, UsageProvider,
};
use crate::reset;
use aes::cipher::{block_padding::Pkcs7, BlockModeDecrypt, KeyIvInit};
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use jiff::tz::TimeZone;
use jiff::Timestamp;
use rusqlite::{Connection, OpenFlags};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const DEFAULT_CURSOR_USAGE_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage";

pub struct CursorProvider;

impl Default for CursorProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorProvider {
    pub fn new() -> Self {
        Self
    }
}

impl UsageProvider for CursorProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            id: "cursor",
            display_name: "Cursor",
            icon_name: "provider-cursor",
            dashboard_url: "https://cursor.com/dashboard",
            default_ttl_secs: 300,
        }
    }

    fn discover_accounts(
        &self,
        manual: &[ManualAccountConfig],
        auto_discover: bool,
        ctx: &FetchContext,
    ) -> Vec<AccountDescriptor> {
        let mut out = Vec::new();
        let home = ctx.home_dir.as_deref();

        for (idx, m) in manual.iter().enumerate() {
            let expanded = m
                .path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|p| expand_tilde_with_home(p, home));
            let source_label = expanded.as_deref().map(|p| abbreviate_home(p, home));

            out.push(AccountDescriptor {
                provider_id: "cursor".to_string(),
                id: format!("cursor:{}", m.id),
                label: m.display_label(),
                dedup_key: expanded.as_ref().map(|p| p.to_string_lossy().into_owned()),
                path: expanded,
                source_label,
                binary: m.binary.clone(),
                is_system_default: idx == 0,
                poll_mode: m.poll_mode,
                login_command: Some("cursor".to_string()),
                switch_command: m.switch_command.clone(),
            });
        }

        if auto_discover {
            let mut has_env_token = false;
            if let Some(env_tok) = ctx.env_var("CURSOR_ACCESS_TOKEN") {
                has_env_token = true;
                let email = extract_cursor_jwt_email(&env_tok);
                out.push(AccountDescriptor {
                    provider_id: "cursor".to_string(),
                    id: "cursor:env".to_string(),
                    label: email.clone().unwrap_or_else(|| "Cursor (env)".to_string()),
                    dedup_key: email.map(|e| e.to_ascii_lowercase()),
                    path: None,
                    source_label: Some("CURSOR_ACCESS_TOKEN".to_string()),
                    binary: None,
                    is_system_default: out.is_empty(),
                    poll_mode: PollMode::Active,
                    login_command: Some("cursor".to_string()),
                    switch_command: None,
                });
            }

            if !has_env_token {
                for loc in ctx.discover_locations("cursor") {
                    out.push(AccountDescriptor {
                        provider_id: "cursor".to_string(),
                        id: format!("cursor:{}", loc.suggested_id),
                        label: "Cursor".to_string(),
                        dedup_key: Some(loc.path.to_string_lossy().into_owned()),
                        path: Some(loc.path),
                        source_label: Some(loc.source_label),
                        binary: None,
                        is_system_default: loc.is_system_default && out.is_empty(),
                        poll_mode: loc.poll_mode,
                        login_command: Some("cursor".to_string()),
                        switch_command: None,
                    });
                }
            }
        }

        out
    }

    fn fetch_account(
        &self,
        account: &AccountDescriptor,
        _cached: Option<&CachedAccountState>,
        ctx: &FetchContext,
    ) -> AccountFetchOutcome {
        let auth_res = resolve_cursor_auth(account, ctx);
        let auth_info = match auth_res {
            Ok(info) => info,
            Err(CursorAuthError::EncryptedKeyring { email, plan_tier }) => {
                return AccountFetchOutcome::Error {
                    message: format!(
                        "Encrypted keyring token{}; set CURSOR_ACCESS_TOKEN or configure a JSON token file",
                        email
                            .as_deref()
                            .map(|e| format!(" for {e}"))
                            .unwrap_or_default()
                    ),
                }
                .with_optional_meta(email, plan_tier);
            }
            Err(CursorAuthError::Missing(msg)) => {
                return AccountFetchOutcome::SignedOut {
                    message: msg,
                    login_command: account.login_command.clone(),
                    label: None,
                    plan_tier: None,
                };
            }
            Err(CursorAuthError::Failed(msg)) => {
                return AccountFetchOutcome::Error { message: msg };
            }
        };

        let resolved_label = if account.label == "Cursor" || account.label == "default" {
            auth_info
                .email
                .clone()
                .unwrap_or_else(|| account.label.clone())
        } else {
            account.label.clone()
        };

        let usage_url = ctx
            .endpoints
            .cursor_usage_url
            .as_deref()
            .unwrap_or(DEFAULT_CURSOR_USAGE_URL);
        let bearer = format!("Bearer {}", auth_info.access_token);
        let headers = [
            ("Authorization", bearer.as_str()),
            ("Connect-Protocol-Version", "1"),
            ("Accept", "application/json"),
        ];

        match ctx.http.post_json(usage_url, &headers, "{}") {
            Ok(resp) if resp.is_success() => {
                match parse_cursor_usage_json(&resp.body, ctx.now, &TimeZone::system()) {
                    Ok(parsed) => AccountFetchOutcome::Success(AccountReport {
                        id: account.id.clone(),
                        label: resolved_label,
                        source_label: account.source_label.clone(),
                        is_system_default: account.is_system_default,
                        is_active: false,
                        plan_tier: parsed.plan_tier.or(auth_info.plan_tier),
                        state: AccountState::ActiveOk,
                        is_stale: false,
                        status_note: None,
                        extra_info: None,
                        error: None,
                        login_command: account.login_command.clone(),
                        updated_at_epoch: Some(ctx.now_epoch()),
                        buckets: parsed.buckets,
                        credits: parsed.credits,
                        sparkline: Vec::new(),
                    }),
                    Err(err) => AccountFetchOutcome::Error {
                        message: format!("{:#}", err),
                    },
                }
            }
            Ok(resp) if resp.status == 429 => AccountFetchOutcome::RateLimited {
                retry_after_secs: resp.retry_after,
            },
            Ok(resp) if resp.status == 401 || resp.status == 403 => {
                AccountFetchOutcome::SignedOut {
                    message: format!("Cursor usage API returned HTTP {}", resp.status),
                    login_command: account.login_command.clone(),
                    label: Some(resolved_label),
                    plan_tier: auth_info.plan_tier,
                }
            }
            Ok(resp) => AccountFetchOutcome::Error {
                message: format!("Cursor usage API returned HTTP {}", resp.status),
            },
            Err(err) => AccountFetchOutcome::Error {
                message: format!("{:#}", err),
            },
        }
    }
}

trait OutcomeMetaExt {
    fn with_optional_meta(self, email: Option<String>, plan: Option<String>) -> Self;
}

impl OutcomeMetaExt for AccountFetchOutcome {
    fn with_optional_meta(self, email: Option<String>, plan: Option<String>) -> Self {
        match self {
            AccountFetchOutcome::Error { message } => AccountFetchOutcome::ErrorWithMeta {
                message,
                label: email,
                plan_tier: plan,
            },
            other => other,
        }
    }
}

// ---------------------------------------------------------------------------
// Zero-C-Dep `state.vscdb` & JSON Token File Credential Reader
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct CursorAuthInfo {
    pub access_token: String,
    pub email: Option<String>,
    pub plan_tier: Option<String>,
}

impl std::fmt::Debug for CursorAuthInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CursorAuthInfo")
            .field("access_token", &"[REDACTED]")
            .field("email", &self.email)
            .field("plan_tier", &self.plan_tier)
            .finish()
    }
}

#[derive(Debug)]
pub enum CursorAuthError {
    EncryptedKeyring {
        email: Option<String>,
        plan_tier: Option<String>,
    },
    Missing(String),
    Failed(String),
}

fn resolve_cursor_auth(
    account: &AccountDescriptor,
    ctx: &FetchContext,
) -> std::result::Result<CursorAuthInfo, CursorAuthError> {
    if account.source_label.as_deref() == Some("CURSOR_ACCESS_TOKEN") {
        if let Some(tok) = ctx.env_var("CURSOR_ACCESS_TOKEN") {
            return Ok(CursorAuthInfo {
                email: extract_cursor_jwt_email(&tok),
                access_token: tok,
                plan_tier: None,
            });
        }
    }

    if let Some(ref p) = account.path {
        let expanded_p = match p.to_str() {
            Some(s) => expand_tilde_with_home(s, ctx.home_dir.as_deref()),
            None => p.clone(),
        };
        return read_cursor_auth_from_path_with_ctx(&expanded_p, ctx.timeout, Some(ctx)).or_else(
            |err| {
                // Allow `CURSOR_ACCESS_TOKEN` env fallback when `state.vscdb` is keyring-encrypted!
                if matches!(err, CursorAuthError::EncryptedKeyring { .. }) {
                    if let Some(tok) = ctx.env_var("CURSOR_ACCESS_TOKEN") {
                        let (email, plan_tier) = match err {
                            CursorAuthError::EncryptedKeyring { email, plan_tier } => {
                                (email, plan_tier)
                            }
                            _ => (None, None),
                        };
                        return Ok(CursorAuthInfo {
                            email: email.or_else(|| extract_cursor_jwt_email(&tok)),
                            access_token: tok,
                            plan_tier,
                        });
                    }
                }
                Err(err)
            },
        );
    }

    if let Some(tok) = ctx.env_var("CURSOR_ACCESS_TOKEN") {
        return Ok(CursorAuthInfo {
            email: extract_cursor_jwt_email(&tok),
            access_token: tok,
            plan_tier: None,
        });
    }

    Err(CursorAuthError::Missing(
        "Cursor state.vscdb or CURSOR_ACCESS_TOKEN not found".to_string(),
    ))
}

pub fn read_cursor_auth_from_path(
    path: &Path,
    timeout: Duration,
) -> std::result::Result<CursorAuthInfo, CursorAuthError> {
    read_cursor_auth_from_path_with_ctx(path, timeout, None)
}

fn read_cursor_auth_from_path_with_ctx(
    path: &Path,
    timeout: Duration,
    ctx: Option<&FetchContext>,
) -> std::result::Result<CursorAuthInfo, CursorAuthError> {
    let target = resolve_cursor_target_file(path)?;
    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    // Support SQLite `.vscdb` / `.db` / `.sqlite` OR JSON/text token files!
    if ext == "vscdb" || ext == "db" || ext == "sqlite" || is_sqlite_header(&target) {
        read_cursor_vscdb_with_ctx(&target, timeout, ctx)
    } else {
        read_cursor_token_file(&target)
    }
}

fn resolve_cursor_target_file(path: &Path) -> std::result::Result<PathBuf, CursorAuthError> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if path.is_dir() {
        let candidates = [
            path.join("state.vscdb"),
            path.join("User/globalStorage/state.vscdb"),
            path.join("auth.json"),
            path.join("token.json"),
        ];
        for c in candidates {
            if c.is_file() {
                return Ok(c);
            }
        }
    }
    Err(CursorAuthError::Missing(format!(
        "Cursor credential file not found at {}",
        path.display()
    )))
}

fn is_sqlite_header(path: &Path) -> bool {
    use std::io::Read;
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut hdr = Vec::with_capacity(16);
    if file.take(16).read_to_end(&mut hdr).is_err() {
        return false;
    }
    hdr.starts_with(b"SQLite format 3\0")
}

/// Parses a JSON or plain-text Cursor token file (`{"accessToken": "...", "email": "...", "membershipType": "ultra"}`).
pub fn read_cursor_token_file(
    file_path: &Path,
) -> std::result::Result<CursorAuthInfo, CursorAuthError> {
    let raw = fs::read_to_string(file_path).map_err(|e| {
        CursorAuthError::Failed(format!("Failed to read {}: {e}", file_path.display()))
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CursorAuthError::Missing(format!(
            "Cursor token file {} is empty",
            file_path.display()
        )));
    }

    if trimmed.starts_with('{') {
        let val: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
            CursorAuthError::Failed(format!("Invalid JSON in {}: {e}", file_path.display()))
        })?;
        let token = val
            .get("accessToken")
            .or_else(|| val.get("access_token"))
            .or_else(|| val.get("cursorAuth/accessToken"))
            .or_else(|| val.get("token"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CursorAuthError::Missing(format!("No accessToken found in {}", file_path.display()))
            })?;
        let email = val
            .get("email")
            .or_else(|| val.get("cachedEmail"))
            .or_else(|| val.get("cursorAuth/cachedEmail"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| extract_cursor_jwt_email(token));
        let plan_tier = val
            .get("membershipType")
            .or_else(|| val.get("stripeMembershipType"))
            .or_else(|| val.get("cursorAuth/stripeMembershipType"))
            .or_else(|| val.get("plan"))
            .and_then(|v| v.as_str())
            .and_then(normalize_cursor_plan_badge);

        return Ok(CursorAuthInfo {
            access_token: token.to_string(),
            email,
            plan_tier,
        });
    }

    // Plain single-line JWT/token file
    let first_line = trimmed.lines().next().unwrap_or_default().trim();
    Ok(CursorAuthInfo {
        access_token: first_line.to_string(),
        email: extract_cursor_jwt_email(first_line),
        plan_tier: None,
    })
}

/// Queries Cursor's `state.vscdb` in-process via bundled `rusqlite` in read-only immutable mode
/// (`file:<path>?mode=ro&immutable=1`) with an isolated `0600` temp-copy (+ `-wal`) fallback when
/// uncheckpointed WAL frames or concurrent writes require replay.
pub fn read_cursor_vscdb(
    db_path: &Path,
    timeout: Duration,
) -> std::result::Result<CursorAuthInfo, CursorAuthError> {
    read_cursor_vscdb_with_ctx(db_path, timeout, None)
}

fn read_cursor_vscdb_with_ctx(
    db_path: &Path,
    timeout: Duration,
    ctx: Option<&FetchContext>,
) -> std::result::Result<CursorAuthInfo, CursorAuthError> {
    let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
    let has_nonempty_wal = fs::metadata(&wal_path).is_ok_and(|m| m.is_file() && m.len() > 0);
    if has_nonempty_wal {
        let rows = query_cursor_vscdb_via_temp_copy(db_path, &wal_path)?;
        return parse_cursor_kv_rows(&rows, || {
            ctx.and_then(|c| lookup_cursor_safe_storage_secret(timeout, c))
        });
    }

    let uri = format!(
        "file:{}?mode=ro&immutable=1",
        encode_sqlite_uri_path(db_path)
    );
    let immutable_flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;

    let immutable_rows = Connection::open_with_flags(&uri, immutable_flags)
        .and_then(|conn| query_cursor_item_table(&conn));

    let rows = match immutable_rows {
        Ok(r) => r,
        Err(_) => query_cursor_vscdb_via_temp_copy(db_path, &wal_path)?,
    };

    parse_cursor_kv_rows(&rows, || {
        ctx.and_then(|c| lookup_cursor_safe_storage_secret(timeout, c))
    })
}

fn encode_sqlite_uri_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3f"),
            '#' => out.push_str("%23"),
            c => out.push(c),
        }
    }
    out
}

fn query_cursor_item_table(
    conn: &Connection,
) -> std::result::Result<Vec<(String, Vec<u8>)>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT key, CAST(value AS BLOB) FROM ItemTable WHERE key IN ('cursorAuth/accessToken', 'cursorAuth/cachedEmail', 'cursorAuth/stripeMembershipType')",
    )?;
    let iter = stmt.query_map([], |row| {
        let key: String = row.get(0)?;
        let val: Vec<u8> = row.get(1)?;
        Ok((key, val))
    })?;
    let mut out = Vec::new();
    for item in iter {
        out.push(item?);
    }
    Ok(out)
}

fn query_cursor_vscdb_via_temp_copy(
    db_path: &Path,
    wal_path: &Path,
) -> std::result::Result<Vec<(String, Vec<u8>)>, CursorAuthError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CURSOR_TMP_NONCE: AtomicU64 = AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = CURSOR_TMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let tmp_dir = std::env::temp_dir().join(format!(
        ".gas-cursor-vscdb-{}-{seq}-{nanos}",
        std::process::id()
    ));

    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.recursive(false).mode(0o700);
        builder.create(&tmp_dir).map_err(|e| {
            CursorAuthError::Failed(format!(
                "Failed to create isolated 0700 temp directory {}: {e}",
                tmp_dir.display()
            ))
        })?;
        let meta = fs::symlink_metadata(&tmp_dir).map_err(|e| {
            let _ = fs::remove_dir_all(&tmp_dir);
            CursorAuthError::Failed(format!("Failed to inspect {}: {e}", tmp_dir.display()))
        })?;
        // SAFETY: `libc::geteuid()` is a pure POSIX syscall with no pointer arguments.
        let euid = unsafe { libc::geteuid() };
        if meta.file_type().is_symlink() || !meta.file_type().is_dir() || meta.uid() != euid {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err(CursorAuthError::Failed(format!(
                "Refusing unsafe temp directory {}",
                tmp_dir.display()
            )));
        }
        let _ = fs::set_permissions(&tmp_dir, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(&tmp_dir).map_err(|e| {
            CursorAuthError::Failed(format!(
                "Failed to create temp directory {}: {e}",
                tmp_dir.display()
            ))
        })?;
    }

    let tmp_db = tmp_dir.join("state.vscdb");
    let tmp_wal = tmp_dir.join("state.vscdb-wal");
    let tmp_shm = tmp_dir.join("state.vscdb-shm");

    if let Err(e) = copy_file_0600(db_path, &tmp_db) {
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(CursorAuthError::Failed(format!(
            "Failed to copy {}: {e}",
            db_path.display()
        )));
    }
    if wal_path.is_file() {
        let _ = copy_file_0600(wal_path, &tmp_wal);
    }

    // Open with RW|NO_MUTEX ONLY on our isolated `0700` private temp copy when `-wal` is present so SQLite
    // can replay `-wal` frames without needing a pre-existing `-shm` file; otherwise RO|NO_MUTEX.
    let flags = if tmp_wal.is_file() {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
    };
    let res =
        Connection::open_with_flags(&tmp_db, flags).and_then(|conn| query_cursor_item_table(&conn));

    let _ = fs::remove_file(&tmp_db);
    let _ = fs::remove_file(&tmp_wal);
    let _ = fs::remove_file(&tmp_shm);
    let _ = fs::remove_dir(&tmp_dir);

    res.map_err(|e| {
        CursorAuthError::Failed(format!(
            "SQLite query failed for {}: {e}",
            db_path.display()
        ))
    })
}

fn copy_file_0600(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::io::{Read, Write};
    const MAX_VSCDB_COPY_BYTES: u64 = 512 * 1024 * 1024;

    if let Ok(meta) = fs::symlink_metadata(dst) {
        if meta.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Refusing to write through symbolic link",
            ));
        }
    }
    let src_file = fs::File::open(src)?;
    let src_len = src_file.metadata().map(|m| m.len()).unwrap_or(0);
    if src_len > MAX_VSCDB_COPY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "state.vscdb exceeded 512 MiB safety cap",
        ));
    }

    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = opts.open(dst)?;
    #[cfg(unix)]
    {
        let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
    }
    let mut reader = src_file.take(MAX_VSCDB_COPY_BYTES);
    std::io::copy(&mut reader, &mut file)?;
    file.flush()
}

/// Pure-Rust parser for `key=HEX_BYTES` lines returned from `ItemTable`.
pub fn parse_cursor_sqlite_hex_rows(
    raw_lines: &str,
) -> std::result::Result<CursorAuthInfo, CursorAuthError> {
    let mut rows = Vec::new();
    for line in raw_lines.lines() {
        let Some((key, hex_val)) = line.trim().split_once('=') else {
            continue;
        };
        let Some(bytes) = decode_hex(hex_val.trim()) else {
            continue;
        };
        rows.push((key.to_string(), bytes));
    }
    parse_cursor_kv_rows(&rows, || None)
}

fn parse_cursor_kv_rows<F>(
    rows: &[(String, Vec<u8>)],
    secret_lookup: F,
) -> std::result::Result<CursorAuthInfo, CursorAuthError>
where
    F: FnOnce() -> Option<Vec<u8>>,
{
    let mut token_bytes: Option<Vec<u8>> = None;
    let mut email: Option<String> = None;
    let mut plan_tier: Option<String> = None;

    for (key, bytes) in rows {
        match key.as_str() {
            "cursorAuth/accessToken" => {
                token_bytes = Some(bytes.clone());
            }
            "cursorAuth/cachedEmail" => {
                let s = String::from_utf8_lossy(bytes)
                    .trim()
                    .trim_matches('"')
                    .to_string();
                if !s.is_empty() {
                    email = Some(s);
                }
            }
            "cursorAuth/stripeMembershipType" => {
                let s = String::from_utf8_lossy(bytes)
                    .trim()
                    .trim_matches('"')
                    .to_string();
                plan_tier = normalize_cursor_plan_badge(&s);
            }
            _ => {}
        }
    }

    let Some(raw_tok) = token_bytes else {
        return Err(CursorAuthError::Missing(
            "Not signed in to Cursor (cursorAuth/accessToken missing)".to_string(),
        ));
    };

    // Electron `safeStorage` (`v10` / `v11` prefix or non-UTF8 binary ciphertext)
    let token_str = if raw_tok.starts_with(b"v10") || raw_tok.starts_with(b"v11") {
        match decrypt_chromium_safe_storage_with_lookup(&raw_tok, secret_lookup) {
            Some(decrypted) => decrypted,
            None => {
                return Err(CursorAuthError::EncryptedKeyring { email, plan_tier });
            }
        }
    } else if let Ok(utf8) = std::str::from_utf8(&raw_tok) {
        utf8.trim().trim_matches('"').to_string()
    } else {
        return Err(CursorAuthError::EncryptedKeyring { email, plan_tier });
    };

    if token_str.is_empty() {
        return Err(CursorAuthError::Missing(
            "Cursor accessToken is empty".to_string(),
        ));
    }

    if email.is_none() {
        email = extract_cursor_jwt_email(&token_str);
    }

    Ok(CursorAuthInfo {
        access_token: token_str,
        email,
        plan_tier,
    })
}

/// Decrypts Chromium/Electron `safeStorage` (`v10` `"peanuts"` or `v11` libsecret password)
/// ciphertext strictly in memory (`PBKDF2-HMAC-SHA1(password, "saltysalt", 1)` + `AES-128-CBC`
/// with 16-space IV and PKCS#7 padding).
pub fn decrypt_chromium_safe_storage_with_secret(
    raw_tok: &[u8],
    v11_secret: Option<&[u8]>,
) -> Option<String> {
    decrypt_chromium_safe_storage_with_lookup(raw_tok, || v11_secret.map(|s| s.to_vec()))
}

fn decrypt_chromium_safe_storage_with_lookup<F>(raw_tok: &[u8], secret_lookup: F) -> Option<String>
where
    F: FnOnce() -> Option<Vec<u8>>,
{
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

    let (version, payload) = if let Some(rest) = raw_tok.strip_prefix(b"v10") {
        (10u8, rest)
    } else {
        let rest = raw_tok.strip_prefix(b"v11")?;
        (11u8, rest)
    };

    if payload.is_empty() || !payload.len().is_multiple_of(16) {
        return None;
    }

    let password: Vec<u8> = if version == 10 {
        b"peanuts".to_vec()
    } else {
        secret_lookup()?
    };

    let key = pbkdf2::pbkdf2_hmac_array::<sha1::Sha1, 16>(&password, b"saltysalt", 1);
    let iv = [b' '; 16];
    let mut buf = payload.to_vec();
    let decrypted = Aes128CbcDec::new(&key.into(), &iv.into())
        .decrypt_padded::<Pkcs7>(&mut buf)
        .ok()?;

    extract_clean_decrypted_token(decrypted)
}

fn extract_clean_decrypted_token(decrypted: &[u8]) -> Option<String> {
    if let Ok(s) = std::str::from_utf8(decrypted) {
        let trimmed = s.trim().trim_matches('"');
        if !trimmed.is_empty()
            && !trimmed
                .chars()
                .any(|c| c.is_control() && !c.is_ascii_whitespace())
        {
            return Some(trimmed.to_string());
        }
    }
    // Chromium 130+ / Electron domain-bound prefix (32-byte SHA-256 hash before UTF-8 token)
    if decrypted.len() > 32 {
        if let Ok(s) = std::str::from_utf8(&decrypted[32..]) {
            let trimmed = s.trim().trim_matches('"');
            if trimmed.len() >= 12 && !trimmed.chars().any(char::is_control) && trimmed.is_ascii() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Queries `org.freedesktop.secrets` for Cursor's `chrome_libsecret_os_crypt_password_v2` password
/// ONLY from already-unlocked keyring items (`SearchItems` `unlocked` array — NEVER calling `Unlock`
/// or `secret-tool lookup`, which passes `SECRET_SEARCH_UNLOCK` and pops modal password dialogs).
fn lookup_cursor_safe_storage_secret(timeout: Duration, ctx: &FetchContext) -> Option<Vec<u8>> {
    // Respect hermetic test contexts unless DBUS_SESSION_BUS_ADDRESS is explicitly provided.
    if ctx.env_overrides.is_some() && ctx.env_var("DBUS_SESSION_BUS_ADDRESS").is_none() {
        return None;
    }

    let bounded = timeout.min(Duration::from_secs(2));

    // 1. Primary on GNOME systems: single-connection `gjs` D-Bus query reading ONLY `unlocked` items.
    let gjs_bin = ctx.resolve_binary("gjs", None);
    if gjs_bin.is_file() {
        const GJS_SCRIPT: &str = concat!(
            "const {Gio, GLib} = imports.gi;",
            "const bus = Gio.DBus.session;",
            "const [, sessPath] = bus.call_sync('org.freedesktop.secrets','/org/freedesktop/secrets','org.freedesktop.Secret.Service','OpenSession',new GLib.Variant('(sv)',['plain',new GLib.Variant('s','')]),new GLib.VariantType('(vo)'),Gio.DBusCallFlags.NONE,1500,null).deep_unpack();",
            "try {",
            "  for (const attrs of [{'application':'Cursor','xdg:schema':'chrome_libsecret_os_crypt_password_v2'},{'application':'cursor','xdg:schema':'chrome_libsecret_os_crypt_password_v2'},{'application':'Cursor'}]) {",
            "    const [unlocked] = bus.call_sync('org.freedesktop.secrets','/org/freedesktop/secrets','org.freedesktop.Secret.Service','SearchItems',new GLib.Variant('(a{ss})',[attrs]),new GLib.VariantType('(aoao)'),Gio.DBusCallFlags.NONE,1500,null).deep_unpack();",
            "    if (unlocked.length > 0) {",
            "      const [secDict] = bus.call_sync('org.freedesktop.secrets','/org/freedesktop/secrets','org.freedesktop.Secret.Service','GetSecrets',new GLib.Variant('(aoo)',[[unlocked[0]],sessPath]),new GLib.VariantType('(a{o(oayays)})'),Gio.DBusCallFlags.NONE,1500,null).deep_unpack();",
            "      const entry = secDict[unlocked[0]];",
            "      if (entry && entry[2] && entry[2].length > 0) { print(GLib.base64_encode(entry[2])); break; }",
            "    }",
            "  }",
            "} finally {",
            "  try { bus.call_sync('org.freedesktop.secrets',sessPath,'org.freedesktop.Secret.Session','Close',null,null,Gio.DBusCallFlags.NONE,500,null); } catch (_) {}",
            "}"
        );
        let spec = CommandSpec::new(&gjs_bin, bounded).args(["-c", GJS_SCRIPT]);
        if let Ok(out) = run_command(&spec) {
            if out.status.success() {
                if let Some(b64_line) = out.stdout.lines().map(str::trim).find(|l| !l.is_empty()) {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64_line) {
                        if !bytes.is_empty() {
                            return Some(bytes);
                        }
                    }
                }
            }
        }
    }

    // 2. Fallback for non-GNOME Waybar/tmux setups (Sway, Hyprland, KDE) via Python3 GI/DBus unlocked-only query.
    let py_bin = ctx.resolve_binary("python3", None);
    if py_bin.is_file() {
        const PY_SCRIPT: &str = concat!(
            "import base64\n",
            "from gi.repository import Gio, GLib\n",
            "bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)\n",
            "_, sess = bus.call_sync('org.freedesktop.secrets','/org/freedesktop/secrets','org.freedesktop.Secret.Service','OpenSession',GLib.Variant('(sv)',('plain',GLib.Variant('s',''))),GLib.VariantType('(vo)'),Gio.DBusCallFlags.NONE,1500,None).unpack()\n",
            "try:\n",
            "  for attrs in ({'application':'Cursor','xdg:schema':'chrome_libsecret_os_crypt_password_v2'},{'application':'cursor','xdg:schema':'chrome_libsecret_os_crypt_password_v2'},{'application':'Cursor'}):\n",
            "    unlocked, _ = bus.call_sync('org.freedesktop.secrets','/org/freedesktop/secrets','org.freedesktop.Secret.Service','SearchItems',GLib.Variant('(a{ss})',(attrs,)),GLib.VariantType('(aoao)'),Gio.DBusCallFlags.NONE,1500,None).unpack()\n",
            "    if unlocked:\n",
            "      sec, = bus.call_sync('org.freedesktop.secrets','/org/freedesktop/secrets','org.freedesktop.Secret.Service','GetSecrets',GLib.Variant('(aoo)',([unlocked[0]],sess)),GLib.VariantType('(a{o(oayays)})'),Gio.DBusCallFlags.NONE,1500,None).unpack()\n",
            "      entry = sec.get(unlocked[0])\n",
            "      if entry and entry[2]:\n",
            "        print(base64.b64encode(bytes(entry[2])).decode('ascii'))\n",
            "        break\n",
            "finally:\n",
            "  try: bus.call_sync('org.freedesktop.secrets',sess,'org.freedesktop.Secret.Session','Close',None,None,Gio.DBusCallFlags.NONE,500,None)\n",
            "  except Exception: pass\n"
        );
        let spec = CommandSpec::new(&py_bin, bounded).args(["-c", PY_SCRIPT]);
        if let Ok(out) = run_command(&spec) {
            if out.status.success() {
                if let Some(b64_line) = out.stdout.lines().map(str::trim).find(|l| !l.is_empty()) {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64_line) {
                        if !bytes.is_empty() {
                            return Some(bytes);
                        }
                    }
                }
            }
        }
    }

    None
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Some(out)
}

fn extract_cursor_jwt_email(token: &str) -> Option<String> {
    let claims = decode_jwt_payload(token)?;
    claims
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| s.contains('@'))
        .map(String::from)
}

pub fn normalize_cursor_plan_badge(raw: &str) -> Option<String> {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.as_str() {
        "" => None,
        "ultra" => Some("Ultra".to_string()),
        "pro_plus" | "pro+" | "proplus" => Some("Pro+".to_string()),
        "pro" => Some("Pro".to_string()),
        "business" | "team" | "enterprise" => Some("Business".to_string()),
        "free" | "hobby" | "free_trial" => Some("Free".to_string()),
        _ if lower.contains("ultra") => Some("Ultra".to_string()),
        _ if lower.contains("pro") => Some("Pro".to_string()),
        _ if lower.contains("business") || lower.contains("enterprise") => {
            Some("Business".to_string())
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Connect-RPC `GetCurrentPeriodUsage` Response Parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ParsedCursorUsage {
    pub plan_tier: Option<String>,
    pub buckets: Vec<UsageBucket>,
    pub credits: Option<CreditSummary>,
}

/// Parses Cursor `GetCurrentPeriodUsage` (or `usage-summary`) JSON into the 3 canonical buckets:
/// 1. `Cursor Models` (`autoPercentUsed`, `inline_pair`, `is_primary: false`)
/// 2. `Other Models` (`apiPercentUsed`, `inline_pair`, `is_primary: false`)
/// 3. `31d` (`totalPercentUsed`, `inline_pair`, `is_primary: true`)
pub fn parse_cursor_usage_json(
    raw: &str,
    now: Timestamp,
    tz: &TimeZone,
) -> Result<ParsedCursorUsage> {
    let val: serde_json::Value =
        serde_json::from_str(raw).context("Failed to parse Cursor usage JSON")?;

    let plan_usage = val
        .get("planUsage")
        .or_else(|| val.pointer("/individualUsage/plan"))
        .ok_or_else(|| anyhow!("Missing 'planUsage' in Cursor usage response"))?;

    let auto_pct = plan_usage
        .get("autoPercentUsed")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let api_pct = plan_usage
        .get("apiPercentUsed")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let total_pct = plan_usage
        .get("totalPercentUsed")
        .and_then(|v| v.as_f64())
        .or_else(|| {
            let spend = plan_usage
                .get("totalSpend")
                .or_else(|| plan_usage.get("used"))
                .and_then(|v| v.as_f64())?;
            let limit = plan_usage.get("limit").and_then(|v| v.as_f64())?;
            if limit > 0.0 {
                Some((spend / limit) * 100.0)
            } else {
                Some(0.0)
            }
        })
        .unwrap_or(0.0);

    // Billing cycle end timestamp (may be integer ms/s in `billingCycleEnd` or ISO string in `billingCycle.end`)
    let reset_norm = extract_cursor_cycle_end(&val, now, tz);

    let buckets = vec![
        UsageBucket::new(
            "cursor-models",
            "Cursor Models",
            "Cursor Models",
            BucketLayout::InlinePair,
            false,
            auto_pct,
            reset_norm.clone(),
        ),
        UsageBucket::new(
            "other-models",
            "Other Models",
            "Other Models",
            BucketLayout::InlinePair,
            false,
            api_pct,
            reset_norm.clone(),
        ),
        UsageBucket::new(
            "31d",
            "Monthly",
            "31d",
            BucketLayout::InlinePair,
            true,
            total_pct,
            reset_norm,
        ),
    ];

    let plan_tier = val
        .get("membershipType")
        .or_else(|| val.get("stripeMembershipType"))
        .or_else(|| val.get("planName"))
        .and_then(|v| v.as_str())
        .and_then(normalize_cursor_plan_badge);

    // Optional on-demand / spend limit summary (Cursor reports `spendLimitUsage` or `onDemand` in cents)
    let credits = val
        .get("spendLimitUsage")
        .or_else(|| val.pointer("/individualUsage/onDemand"))
        .and_then(|od| {
            let used_cents = od
                .get("individualUsed")
                .or_else(|| od.get("pooledUsed"))
                .or_else(|| od.get("used"))
                .and_then(|v| v.as_f64())?;
            let limit_cents = od
                .get("individualLimit")
                .or_else(|| od.get("pooledLimit"))
                .or_else(|| od.get("limit"))
                .and_then(|v| v.as_f64());

            if used_cents <= 0.0 && limit_cents.unwrap_or(0.0) <= 0.0 {
                return None;
            }
            let used_dollars = used_cents / 100.0;
            let limit_dollars = limit_cents.filter(|l| *l > 0.0).map(|l| l / 100.0);
            let used_percent = limit_dollars
                .map(|lim| ((used_dollars / lim) * 100.0).clamp(0.0, 100.0).round() as u8);

            Some(CreditSummary {
                label: "On-demand spend".to_string(),
                used_dollars: Some(used_dollars),
                limit_dollars,
                balance_dollars: None,
                used_percent,
            })
        });

    Ok(ParsedCursorUsage {
        plan_tier,
        buckets,
        credits,
    })
}

fn extract_cursor_cycle_end(
    val: &serde_json::Value,
    now: Timestamp,
    tz: &TimeZone,
) -> Option<(i64, String)> {
    let candidates = [val.get("billingCycleEnd"), val.pointer("/billingCycle/end")];
    for cand in candidates.into_iter().flatten() {
        if let Some(ts) = cand.as_i64() {
            if let Some(norm) = reset::normalize_epoch(ts, tz) {
                return Some(norm);
            }
        }
        if let Some(s) = cand.as_str() {
            if let Some(norm) = reset::normalize_reset(s, now, tz) {
                return Some(norm);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_encode(input: &[u8]) -> String {
        input.iter().map(|b| format!("{b:02X}")).collect()
    }

    #[test]
    fn test_parse_cursor_usage_buckets_and_plan_badges() {
        let now = crate::reset::utc_ts(2026, 11, 10, 18, 0, 0);
        let raw = r#"{
          "membershipType": "ultra",
          "billingCycleEnd": 1794528000000,
          "planUsage": {
            "totalPercentUsed": 28.4,
            "autoPercentUsed": 12.2,
            "apiPercentUsed": 41.7,
            "totalSpend": 5680,
            "limit": 20000
          },
          "spendLimitUsage": {
            "individualUsed": 1250,
            "individualLimit": 5000
          }
        }"#;

        let parsed = parse_cursor_usage_json(raw, now, &TimeZone::UTC).expect("parse cursor usage");
        assert_eq!(parsed.plan_tier.as_deref(), Some("Ultra"));
        assert_eq!(parsed.buckets.len(), 3);

        assert_eq!(parsed.buckets[0].id, "cursor-models");
        assert_eq!(parsed.buckets[0].label, "Cursor Models");
        assert_eq!(parsed.buckets[0].layout_hint, BucketLayout::InlinePair);
        assert!(!parsed.buckets[0].is_primary);
        assert_eq!(parsed.buckets[0].used_percent, 12);

        assert_eq!(parsed.buckets[1].id, "other-models");
        assert_eq!(parsed.buckets[1].label, "Other Models");
        assert!(!parsed.buckets[1].is_primary);
        assert_eq!(parsed.buckets[1].used_percent, 42);

        assert_eq!(parsed.buckets[2].id, "31d");
        assert_eq!(parsed.buckets[2].short_label, "31d");
        assert!(parsed.buckets[2].is_primary);
        assert_eq!(parsed.buckets[2].used_percent, 28);
        assert_eq!(parsed.buckets[2].resets_at_epoch, Some(1794528000));

        let cred = parsed.credits.expect("spend limit usage");
        assert_eq!(cred.used_dollars, Some(12.5));
        assert_eq!(cred.limit_dollars, Some(50.0));
        assert_eq!(cred.used_percent, Some(25));
    }

    fn encrypt_safe_storage_blob(prefix: &[u8], password: &[u8], plaintext: &[u8]) -> Vec<u8> {
        use aes::cipher::BlockModeEncrypt;
        type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

        let key = pbkdf2::pbkdf2_hmac_array::<sha1::Sha1, 16>(password, b"saltysalt", 1);
        let iv = [b' '; 16];
        let mut buf = vec![0u8; plaintext.len() + 16];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let ct = Aes128CbcEnc::new(&key.into(), &iv.into())
            .encrypt_padded::<Pkcs7>(&mut buf, plaintext.len())
            .expect("encrypt safeStorage payload");
        let mut out = Vec::with_capacity(prefix.len() + ct.len());
        out.extend_from_slice(prefix);
        out.extend_from_slice(ct);
        out
    }

    #[test]
    fn test_cursor_sqlite_hex_parser_and_v10_v11_safe_storage() {
        let jwt = b"eyJhbGciOiJSUzI1NiJ9.eyJlbWFpbCI6ImN1cnNvckBleGFtcGxlLmNvbSJ9.sig";
        let rows = format!(
            "cursorAuth/accessToken={}\ncursorAuth/cachedEmail={}\ncursorAuth/stripeMembershipType={}\n",
            hex_encode(jwt),
            hex_encode(b"cursor@example.com"),
            hex_encode(b"pro")
        );
        let auth = parse_cursor_sqlite_hex_rows(&rows).expect("valid sqlite hex rows");
        assert_eq!(auth.email.as_deref(), Some("cursor@example.com"));
        assert_eq!(auth.plan_tier.as_deref(), Some("Pro"));

        // `v10` ("peanuts") encrypted token decrypts automatically without external keyring!
        let v10_blob = encrypt_safe_storage_blob(b"v10", b"peanuts", jwt);
        let v10_rows = format!("cursorAuth/accessToken={}\n", hex_encode(&v10_blob));
        let v10_auth = parse_cursor_sqlite_hex_rows(&v10_rows).expect("decrypt v10 safeStorage");
        assert_eq!(v10_auth.access_token.as_bytes(), jwt);
        assert_eq!(v10_auth.email.as_deref(), Some("cursor@example.com"));

        // `v11` encrypted token decrypts when keyring secret is supplied
        let v11_secret = b"mock-libsecret-key-24byt";
        let v11_blob = encrypt_safe_storage_blob(b"v11", v11_secret, jwt);
        let decrypted_v11 = decrypt_chromium_safe_storage_with_secret(&v11_blob, Some(v11_secret))
            .expect("decrypt v11 with secret");
        assert_eq!(decrypted_v11.as_bytes(), jwt);
        // Wrong secret or missing secret must fail cleanly
        assert!(decrypt_chromium_safe_storage_with_secret(
            &v11_blob,
            Some(b"wrong-secret-key-000")
        )
        .is_none());
        assert!(decrypt_chromium_safe_storage_with_secret(&v11_blob, None).is_none());

        // Undecryptable `v11` encrypted token must surface EncryptedKeyring with email & plan_tier
        let enc_rows = format!(
            "cursorAuth/accessToken={}\ncursorAuth/cachedEmail={}\ncursorAuth/stripeMembershipType={}\n",
            hex_encode(b"v11\x8a\x9f\x00\xffciphertext"),
            hex_encode(b"locked@example.com"),
            hex_encode(b"ultra")
        );
        let err = parse_cursor_sqlite_hex_rows(&enc_rows).unwrap_err();
        assert!(matches!(
            err,
            CursorAuthError::EncryptedKeyring {
                email: Some(ref e),
                plan_tier: Some(ref p),
            } if e == "locked@example.com" && p == "Ultra"
        ));
    }

    #[test]
    fn test_read_real_sqlite_vscdb_wal_and_json_token_file_without_sqlite3_cli() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "gas-cursor-db-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp_dir).unwrap();

        // 1. JSON token file manual path
        let json_tok_path = tmp_dir.join("cursor-token.json");
        fs::write(
            &json_tok_path,
            r#"{"accessToken":"tok_json_123","email":"json@example.com","membershipType":"ultra"}"#,
        )
        .unwrap();
        let from_json =
            read_cursor_auth_from_path(&json_tok_path, Duration::from_secs(3)).expect("json token");
        assert_eq!(from_json.access_token, "tok_json_123");
        assert_eq!(from_json.email.as_deref(), Some("json@example.com"));
        assert_eq!(from_json.plan_tier.as_deref(), Some("Ultra"));

        // 2. Real SQLite `state.vscdb` created via bundled `rusqlite` (works on stock Ubuntu without `/usr/bin/sqlite3`!)
        let db_path = tmp_dir.join("state.vscdb");
        {
            let conn = Connection::open(&db_path).expect("create fixture state.vscdb");
            conn.execute_batch(concat!(
                "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);",
                "INSERT INTO ItemTable VALUES ('cursorAuth/accessToken', 'eyJhbGciOiJSUzI1NiJ9.eyJlbWFpbCI6InNxbGl0ZUBleGFtcGxlLmNvbSJ9.sig');",
                "INSERT INTO ItemTable VALUES ('cursorAuth/cachedEmail', 'sqlite@example.com');",
                "INSERT INTO ItemTable VALUES ('cursorAuth/stripeMembershipType', 'pro');"
            ))
            .unwrap();
        }
        let from_db =
            read_cursor_auth_from_path(&db_path, Duration::from_secs(3)).expect("vscdb token");
        assert!(from_db.access_token.starts_with("eyJ"));
        assert_eq!(from_db.email.as_deref(), Some("sqlite@example.com"));
        assert_eq!(from_db.plan_tier.as_deref(), Some("Pro"));

        // 3. Uncheckpointed WAL `state.vscdb` + `-wal` while live writer connection stays open
        let wal_db_path = tmp_dir.join("wal_state.vscdb");
        let live_writer = Connection::open(&wal_db_path).expect("open wal state.vscdb");
        live_writer
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA wal_autocheckpoint=0;
                 CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);",
            )
            .unwrap();
        let v10_tok = encrypt_safe_storage_blob(b"v10", b"peanuts", b"tok_wal_v10_789");
        live_writer
            .execute(
                "INSERT INTO ItemTable VALUES ('cursorAuth/accessToken', ?1)",
                [&v10_tok],
            )
            .unwrap();
        live_writer
            .execute(
                "INSERT INTO ItemTable VALUES ('cursorAuth/cachedEmail', 'wal@example.com')",
                [],
            )
            .unwrap();

        let from_wal = read_cursor_vscdb(&wal_db_path, Duration::from_secs(3))
            .expect("read uncheckpointed WAL");
        assert_eq!(from_wal.access_token, "tok_wal_v10_789");
        assert_eq!(from_wal.email.as_deref(), Some("wal@example.com"));

        // Checkpoint base DB with old token, then write refreshed token into `-wal` -> must read fresh `-wal` token!
        live_writer
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        let refreshed_v10 = encrypt_safe_storage_blob(b"v10", b"peanuts", b"tok_wal_refreshed_999");
        live_writer
            .execute(
                "INSERT INTO ItemTable VALUES ('cursorAuth/accessToken', ?1)",
                [&refreshed_v10],
            )
            .unwrap();
        let from_refreshed_wal = read_cursor_vscdb(&wal_db_path, Duration::from_secs(3))
            .expect("read refreshed WAL over checkpointed base DB");
        assert_eq!(from_refreshed_wal.access_token, "tok_wal_refreshed_999");
        drop(live_writer);

        // 4. Undecryptable `v11` in `state.vscdb` preserves email & plan_tier on `ErrorWithMeta`
        let locked_db_path = tmp_dir.join("locked_state.vscdb");
        {
            let conn = Connection::open(&locked_db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
                 INSERT INTO ItemTable VALUES ('cursorAuth/accessToken', X'763131DEADBEEF');
                 INSERT INTO ItemTable VALUES ('cursorAuth/cachedEmail', 'locked@example.com');
                 INSERT INTO ItemTable VALUES ('cursorAuth/stripeMembershipType', 'ultra');",
            )
            .unwrap();
        }
        let provider = CursorProvider::new();
        let desc = AccountDescriptor {
            provider_id: "cursor".to_string(),
            id: "cursor:locked".to_string(),
            label: "Cursor".to_string(),
            dedup_key: None,
            path: Some(locked_db_path),
            source_label: Some("~/.config/Cursor".to_string()),
            binary: None,
            is_system_default: false,
            poll_mode: PollMode::Active,
            login_command: Some("cursor".to_string()),
            switch_command: None,
        };
        let mut ctx = FetchContext::from_config(&crate::config::Config::default());
        ctx.env_overrides = Some(std::collections::BTreeMap::new());
        match provider.fetch_account(&desc, None, &ctx) {
            AccountFetchOutcome::ErrorWithMeta {
                label, plan_tier, ..
            } => {
                assert_eq!(label.as_deref(), Some("locked@example.com"));
                assert_eq!(plan_tier.as_deref(), Some("Ultra"));
            }
            other => panic!("expected ErrorWithMeta, got {:?}", other),
        }

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_cursor_hermetic_env_ignores_ambient_cursor_access_token_and_uses_env_overrides() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use std::collections::BTreeMap;
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let make_jwt = |email: &str| {
            let hdr = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
            let body = URL_SAFE_NO_PAD.encode(format!(r#"{{"email":"{email}"}}"#).as_bytes());
            format!("{hdr}.{body}.sig")
        };
        let ambient_jwt = make_jwt("ambient-poison@cursor.dev");
        let override_jwt = make_jwt("hermetic-override@cursor.dev");

        if std::env::var_os("GAS_HERMETIC_CURSOR_CHILD").is_none() {
            let out = std::process::Command::new(std::env::current_exe().expect("current_exe"))
                .args([
                    "--exact",
                    "providers::cursor::tests::test_cursor_hermetic_env_ignores_ambient_cursor_access_token_and_uses_env_overrides",
                    "--nocapture",
                ])
                .env("GAS_HERMETIC_CURSOR_CHILD", "1")
                .env("CURSOR_ACCESS_TOKEN", &ambient_jwt)
                .output()
                .expect("spawn hermetic cursor test child");
            assert!(
                out.status.success(),
                "hermetic cursor child failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }

        assert_eq!(
            std::env::var("CURSOR_ACCESS_TOKEN").as_deref(),
            Ok(ambient_jwt.as_str()),
            "child must run with ambient CURSOR_ACCESS_TOKEN set in OS environment"
        );

        let tmp = std::env::temp_dir().join(format!(
            "gas-cursor-hermetic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = tmp.join("home");
        let xdg = home.join(".config");
        fs::create_dir_all(&xdg).unwrap();

        let locked_db_path = tmp.join("locked_state.vscdb");
        {
            let conn = Connection::open(&locked_db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
                 INSERT INTO ItemTable VALUES ('cursorAuth/accessToken', X'763131DEADBEEF');
                 INSERT INTO ItemTable VALUES ('cursorAuth/cachedEmail', 'locked@example.com');
                 INSERT INTO ItemTable VALUES ('cursorAuth/stripeMembershipType', 'pro');",
            )
            .unwrap();
        }

        let provider = CursorProvider::new();
        let empty_ctx = FetchContext {
            http: crate::http::HttpClient::new(Duration::from_secs(3)),
            timeout: Duration::from_secs(3),
            now: crate::reset::utc_ts(2026, 11, 15, 12, 0, 0),
            home_dir: Some(home.clone()),
            xdg_config_dir: Some(xdg.clone()),
            proc_root: tmp.join("proc"),
            endpoints: Default::default(),
            env_overrides: Some(BTreeMap::new()),
        };

        // 1. Empty `env_overrides` map MUST ignore ambient `CURSOR_ACCESS_TOKEN` across discovery & all fetch paths!
        assert!(
            provider.discover_accounts(&[], true, &empty_ctx).is_empty(),
            "ambient CURSOR_ACCESS_TOKEN must be ignored when env_overrides is empty"
        );

        let env_desc = AccountDescriptor {
            provider_id: "cursor".to_string(),
            id: "cursor:env".to_string(),
            label: "Cursor".to_string(),
            dedup_key: None,
            path: None,
            source_label: Some("CURSOR_ACCESS_TOKEN".to_string()),
            binary: None,
            is_system_default: true,
            poll_mode: PollMode::Active,
            login_command: Some("cursor".to_string()),
            switch_command: None,
        };
        assert!(matches!(
            provider.fetch_account(&env_desc, None, &empty_ctx),
            AccountFetchOutcome::SignedOut { .. }
        ));

        let manual_no_path_desc = AccountDescriptor {
            source_label: None,
            id: "cursor:manual".to_string(),
            ..env_desc.clone()
        };
        assert!(matches!(
            provider.fetch_account(&manual_no_path_desc, None, &empty_ctx),
            AccountFetchOutcome::SignedOut { .. }
        ));

        let locked_desc = AccountDescriptor {
            id: "cursor:locked".to_string(),
            path: Some(locked_db_path),
            source_label: Some("~/.config/Cursor/state.vscdb".to_string()),
            ..env_desc
        };
        assert!(matches!(
            provider.fetch_account(&locked_desc, None, &empty_ctx),
            AccountFetchOutcome::ErrorWithMeta { .. }
        ));

        // 2. Populated `env_overrides` MUST be used instead of ambient `CURSOR_ACCESS_TOKEN`!
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let expected_bearer = format!("Bearer {override_jwt}");
        let srv = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.contains(&expected_bearer),
                "expected override token in Authorization header, got: {req}"
            );
            let body = r#"{"membershipType":"ultra","planUsage":{"totalPercentUsed":33.0,"autoPercentUsed":10.0,"apiPercentUsed":20.0}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
        });

        let mut env_map = BTreeMap::new();
        env_map.insert("CURSOR_ACCESS_TOKEN".to_string(), override_jwt);
        let override_ctx = FetchContext {
            endpoints: crate::http::EndpointOverrides {
                cursor_usage_url: Some(format!(
                    "http://{addr}/aiserver.v1.DashboardService/GetCurrentPeriodUsage"
                )),
                ..Default::default()
            },
            env_overrides: Some(env_map),
            ..empty_ctx
        };

        let discovered = provider.discover_accounts(&[], true, &override_ctx);
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].id, "cursor:env");
        assert_eq!(discovered[0].label, "hermetic-override@cursor.dev");

        let outcome = provider.fetch_account(&discovered[0], None, &override_ctx);
        srv.join().unwrap();
        match outcome {
            AccountFetchOutcome::Success(rep) => {
                assert_eq!(rep.label, "hermetic-override@cursor.dev");
                assert_eq!(rep.plan_tier.as_deref(), Some("Ultra"));
                assert_eq!(rep.buckets[2].used_percent, 33);
            }
            other => panic!("expected Success with env_overrides token, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&tmp);
    }
}
