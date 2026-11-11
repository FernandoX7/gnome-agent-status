//! Cursor (`cursor`) usage provider:
//! - Zero-C-dependency credential discovery supporting:
//!   1. Read-only `sqlite3` query against `state.vscdb` (`file:...?mode=ro&immutable=1` + `0600` temp-copy fallback),
//!      extracting `cursorAuth/accessToken`, `cursorAuth/cachedEmail`, and `cursorAuth/stripeMembershipType`
//!      (detecting Electron `safeStorage` `v10`/`v11` encrypted tokens cleanly).
//!   2. Manual JSON or plain-text token files (`{"accessToken": "..."}` / `{"cursorAuth/accessToken": "..."}`).
//!   3. `CURSOR_ACCESS_TOKEN` environment variable.
//! - Connect-RPC usage fetch (`POST https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage`),
//!   emitting `Cursor Models`, `Other Models`, and `31d` buckets plus `Ultra` / `Pro` plan badges.

use crate::cache::CachedAccountState;
use crate::config::{
    abbreviate_home, discover_provider_locations, expand_tilde_with_home, resolve_binary_for,
    ManualAccountConfig, PollMode,
};
use crate::http::decode_jwt_payload;
use crate::model::{
    AccountReport, AccountState, BucketLayout, CreditSummary, UsageBucket,
};
use crate::proc_exec::{run_command, CommandSpec};
use crate::providers::{
    AccountDescriptor, AccountFetchOutcome, FetchContext, ProviderMeta, UsageProvider,
};
use crate::reset;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local, TimeZone, Utc};
use std::env;
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
                dedup_key: expanded
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
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
            if let Ok(env_tok) = env::var("CURSOR_ACCESS_TOKEN") {
                if !env_tok.trim().is_empty() {
                    let email = extract_cursor_jwt_email(env_tok.trim());
                    out.push(AccountDescriptor {
                        provider_id: "cursor".to_string(),
                        id: "cursor:env".to_string(),
                        label: email
                            .clone()
                            .unwrap_or_else(|| "Cursor (env)".to_string()),
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
            }

            for loc in discover_provider_locations("cursor", home, ctx.xdg_config_dir.as_deref()) {
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

        out
    }

    fn fetch_account(
        &self,
        account: &AccountDescriptor,
        _cached: Option<&CachedAccountState>,
        ctx: &FetchContext,
    ) -> AccountFetchOutcome {
        let auth_res = resolve_cursor_auth(account, ctx.timeout);
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
                match parse_cursor_usage_json(&resp.body, ctx.now, &Local) {
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
                        error: None,
                        login_command: account.login_command.clone(),
                        updated_at_epoch: Some(ctx.now_epoch()),
                        buckets: parsed.buckets,
                        credits: parsed.credits,
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
    fn with_optional_meta(self, _email: Option<String>, _plan: Option<String>) -> Self;
}

impl OutcomeMetaExt for AccountFetchOutcome {
    fn with_optional_meta(self, _email: Option<String>, _plan: Option<String>) -> Self {
        self
    }
}

// ---------------------------------------------------------------------------
// Zero-C-Dep `state.vscdb` & JSON Token File Credential Reader
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CursorAuthInfo {
    pub access_token: String,
    pub email: Option<String>,
    pub plan_tier: Option<String>,
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
    timeout: Duration,
) -> std::result::Result<CursorAuthInfo, CursorAuthError> {
    if account.source_label.as_deref() == Some("CURSOR_ACCESS_TOKEN") {
        if let Ok(tok) = env::var("CURSOR_ACCESS_TOKEN") {
            let trimmed = tok.trim();
            if !trimmed.is_empty() {
                return Ok(CursorAuthInfo {
                    access_token: trimmed.to_string(),
                    email: extract_cursor_jwt_email(trimmed),
                    plan_tier: None,
                });
            }
        }
    }

    if let Some(ref p) = account.path {
        return read_cursor_auth_from_path(p, timeout).or_else(|err| {
            // Allow `CURSOR_ACCESS_TOKEN` env fallback when `state.vscdb` is keyring-encrypted!
            if matches!(err, CursorAuthError::EncryptedKeyring { .. }) {
                if let Ok(tok) = env::var("CURSOR_ACCESS_TOKEN") {
                    let trimmed = tok.trim();
                    if !trimmed.is_empty() {
                        let (email, plan_tier) = match err {
                            CursorAuthError::EncryptedKeyring { email, plan_tier } => {
                                (email, plan_tier)
                            }
                            _ => (None, None),
                        };
                        return Ok(CursorAuthInfo {
                            access_token: trimmed.to_string(),
                            email: email.or_else(|| extract_cursor_jwt_email(trimmed)),
                            plan_tier,
                        });
                    }
                }
            }
            Err(err)
        });
    }

    if let Ok(tok) = env::var("CURSOR_ACCESS_TOKEN") {
        let trimmed = tok.trim();
        if !trimmed.is_empty() {
            return Ok(CursorAuthInfo {
                access_token: trimmed.to_string(),
                email: extract_cursor_jwt_email(trimmed),
                plan_tier: None,
            });
        }
    }

    Err(CursorAuthError::Missing(
        "Cursor state.vscdb or CURSOR_ACCESS_TOKEN not found".to_string(),
    ))
}

pub fn read_cursor_auth_from_path(
    path: &Path,
    timeout: Duration,
) -> std::result::Result<CursorAuthInfo, CursorAuthError> {
    let target = resolve_cursor_target_file(path)?;
    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    // Support SQLite `.vscdb` / `.db` / `.sqlite` OR JSON/text token files!
    if ext == "vscdb" || ext == "db" || ext == "sqlite" || is_sqlite_header(&target) {
        read_cursor_vscdb(&target, timeout)
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
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    bytes.starts_with(b"SQLite format 3\0")
}

/// Parses a JSON or plain-text Cursor token file (`{"accessToken": "...", "email": "...", "membershipType": "ultra"}`).
pub fn read_cursor_token_file(
    file_path: &Path,
) -> std::result::Result<CursorAuthInfo, CursorAuthError> {
    let raw = fs::read_to_string(file_path)
        .map_err(|e| CursorAuthError::Failed(format!("Failed to read {}: {e}", file_path.display())))?;
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
                CursorAuthError::Missing(format!(
                    "No accessToken found in {}",
                    file_path.display()
                ))
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

/// Queries Cursor's `state.vscdb` in read-only mode via the `sqlite3` CLI (`hex(CAST(value AS BLOB))`)
/// with a `0600` temp-copy fallback when Cursor holds an active WAL lock.
pub fn read_cursor_vscdb(
    db_path: &Path,
    timeout: Duration,
) -> std::result::Result<CursorAuthInfo, CursorAuthError> {
    let sqlite_bin = resolve_binary_for("sqlite3", None);
    let sql = "SELECT key || '=' || hex(CAST(value AS BLOB)) FROM ItemTable WHERE key IN ('cursorAuth/accessToken', 'cursorAuth/cachedEmail', 'cursorAuth/stripeMembershipType');";

    // Attempt 1: Read-only immutable URI (`file:<path>?mode=ro&immutable=1`)
    let uri = format!("file:{}?mode=ro&immutable=1", db_path.display());
    let ro_spec = CommandSpec::new(&sqlite_bin, timeout.min(Duration::from_secs(5)))
        .args(["-batch", "-noheader", &uri, sql]);

    let stdout_text = match run_command(&ro_spec) {
        Ok(out) if out.status.success() && !out.stdout.trim().is_empty() => out.stdout,
        _ => {
            // Attempt 2: Copy `state.vscdb` to an isolated `0600` temp file to avoid WAL contention
            let tmp_db = std::env::temp_dir().join(format!(
                ".gas-cursor-vscdb-{}-{}.sqlite",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            fs::copy(db_path, &tmp_db).map_err(|e| {
                CursorAuthError::Failed(format!("Failed to copy {}: {e}", db_path.display()))
            })?;
            #[cfg(unix)]
            {
                let _ = fs::set_permissions(&tmp_db, fs::Permissions::from_mode(0o600));
            }
            let copy_spec = CommandSpec::new(&sqlite_bin, timeout.min(Duration::from_secs(5)))
                .args([
                    "-batch",
                    "-noheader",
                    tmp_db.to_string_lossy().as_ref(),
                    sql,
                ]);
            let res = run_command(&copy_spec);
            let _ = fs::remove_file(&tmp_db);

            match res {
                Ok(out) if out.status.success() => out.stdout,
                Ok(out) => {
                    return Err(CursorAuthError::Failed(format!(
                        "sqlite3 query failed: {}",
                        out.combined_text().lines().next().unwrap_or("unknown error")
                    )));
                }
                Err(err) => {
                    return Err(CursorAuthError::Failed(format!(
                        "sqlite3 unavailable for {}: {:#}",
                        db_path.display(),
                        err
                    )));
                }
            }
        }
    };

    parse_cursor_sqlite_hex_rows(&stdout_text)
}

/// Pure-Rust parser for `key=HEX_BYTES` lines returned from `ItemTable`.
pub fn parse_cursor_sqlite_hex_rows(
    raw_lines: &str,
) -> std::result::Result<CursorAuthInfo, CursorAuthError> {
    let mut token_bytes: Option<Vec<u8>> = None;
    let mut email: Option<String> = None;
    let mut plan_tier: Option<String> = None;

    for line in raw_lines.lines() {
        let Some((key, hex_val)) = line.trim().split_once('=') else {
            continue;
        };
        let Some(bytes) = decode_hex(hex_val.trim()) else {
            continue;
        };
        match key {
            "cursorAuth/accessToken" => {
                token_bytes = Some(bytes);
            }
            "cursorAuth/cachedEmail" => {
                let s = String::from_utf8_lossy(&bytes)
                    .trim()
                    .trim_matches('"')
                    .to_string();
                if !s.is_empty() {
                    email = Some(s);
                }
            }
            "cursorAuth/stripeMembershipType" => {
                let s = String::from_utf8_lossy(&bytes)
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

    // Detect Electron `safeStorage` (`v10` / `v11` prefix or non-UTF8 binary ciphertext)
    if raw_tok.starts_with(b"v10")
        || raw_tok.starts_with(b"v11")
        || std::str::from_utf8(&raw_tok).is_err()
    {
        return Err(CursorAuthError::EncryptedKeyring { email, plan_tier });
    }

    let token_str = String::from_utf8_lossy(&raw_tok)
        .trim()
        .trim_matches('"')
        .to_string();
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
pub fn parse_cursor_usage_json<T: TimeZone>(
    raw: &str,
    now: DateTime<Utc>,
    tz: &T,
) -> Result<ParsedCursorUsage>
where
    T::Offset: std::fmt::Display,
{
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
            "Monthly (31d)",
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

fn extract_cursor_cycle_end<T: TimeZone>(
    val: &serde_json::Value,
    now: DateTime<Utc>,
    tz: &T,
) -> Option<(i64, String)>
where
    T::Offset: std::fmt::Display,
{
    let candidates = [
        val.get("billingCycleEnd"),
        val.pointer("/billingCycle/end"),
    ];
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
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 18, 0, 0).unwrap();
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

        let parsed = parse_cursor_usage_json(raw, now, &Utc).expect("parse cursor usage");
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

    #[test]
    fn test_cursor_sqlite_hex_parser_and_v10_encrypted_detection() {
        let rows = format!(
            "cursorAuth/accessToken={}\ncursorAuth/cachedEmail={}\ncursorAuth/stripeMembershipType={}\n",
            hex_encode(b"eyJhbGciOiJSUzI1NiJ9.eyJlbWFpbCI6ImN1cnNvckBleGFtcGxlLmNvbSJ9.sig"),
            hex_encode(b"cursor@example.com"),
            hex_encode(b"pro")
        );
        let auth = parse_cursor_sqlite_hex_rows(&rows).expect("valid sqlite hex rows");
        assert_eq!(auth.email.as_deref(), Some("cursor@example.com"));
        assert_eq!(auth.plan_tier.as_deref(), Some("Pro"));

        // Electron safeStorage `v11` encrypted token must be detected cleanly
        let enc_rows = format!(
            "cursorAuth/accessToken={}\ncursorAuth/cachedEmail={}\n",
            hex_encode(b"v11\x8a\x9f\x00\xffciphertext"),
            hex_encode(b"locked@example.com")
        );
        let err = parse_cursor_sqlite_hex_rows(&enc_rows).unwrap_err();
        assert!(matches!(
            err,
            CursorAuthError::EncryptedKeyring { email: Some(ref e), .. } if e == "locked@example.com"
        ));
    }

    #[test]
    fn test_read_real_sqlite_vscdb_and_json_token_file() {
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

        // 2. Real SQLite `state.vscdb` fixture if `sqlite3` CLI is installed on the host
        let sqlite_bin = resolve_binary_for("sqlite3", None);
        if run_command(&CommandSpec::new(&sqlite_bin, Duration::from_secs(2)).args(["-version"]))
            .is_ok_and(|o| o.status.success())
        {
            let db_path = tmp_dir.join("state.vscdb");
            let init_sql = concat!(
                "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);",
                "INSERT INTO ItemTable VALUES ('cursorAuth/accessToken', 'tok_sqlite_456');",
                "INSERT INTO ItemTable VALUES ('cursorAuth/cachedEmail', 'sqlite@example.com');",
                "INSERT INTO ItemTable VALUES ('cursorAuth/stripeMembershipType', 'pro');"
            );
            let create_out = run_command(
                &CommandSpec::new(&sqlite_bin, Duration::from_secs(3))
                    .args([db_path.to_string_lossy().as_ref(), init_sql]),
            )
            .expect("create fixture state.vscdb");
            assert!(create_out.status.success());

            let from_db =
                read_cursor_auth_from_path(&db_path, Duration::from_secs(3)).expect("vscdb token");
            assert_eq!(from_db.access_token, "tok_sqlite_456");
            assert_eq!(from_db.email.as_deref(), Some("sqlite@example.com"));
            assert_eq!(from_db.plan_tier.as_deref(), Some("Pro"));
        }

        let _ = fs::remove_dir_all(&tmp_dir);
    }
}
