//! GitHub Copilot provider (`GET https://api.github.com/copilot_internal/user`).
//!
//! Strictly read-only credential resolution across:
//! 1. `GITHUB_COPILOT_TOKEN` environment variable (system default),
//! 2. Custom `account.path` file (JSON `{oauth_token}` / `hosts.json` / `apps.json`, YAML `hosts.yml`, or plaintext token),
//! 3. `~/.config/github-copilot/hosts.json` or `~/.config/github-copilot/apps.json`,
//! 4. `~/.config/gh/hosts.yml` (inline `oauth_token` or `users.<active>.oauth_token`, with non-interactive
//!    `gh auth token -h github.com` keyring fallback ONLY when `hosts.yml` exists without an inline token).
//!
//! Includes Smart Noise-Free Discovery: when auto-discovered solely via `~/.config/gh/hosts.yml`
//! and `/copilot_internal/user` reports `404` / not subscribed, the card is silently omitted so
//! `gh` users without a Copilot subscription never see false error noise.

use crate::cache::CachedAccountState;
use crate::config::{abbreviate_home, expand_tilde, resolve_binary_for, slugify_id, ManualAccountConfig};
use crate::model::{AccountReport, AccountState, BucketLayout, UsageBucket};
use crate::proc_exec::{self, CommandSpec};
use crate::providers::{
    AccountDescriptor, AccountFetchOutcome, FetchContext, ProviderMeta, UsageProvider,
};
use crate::reset;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local, TimeZone, Utc};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_COPILOT_USER_URL: &str = "https://api.github.com/copilot_internal/user";
const COPILOT_EDITOR_VERSION: &str = "vscode/1.96.0";
const COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.24.0";
const GH_AUTO_ACCOUNT_ID: &str = "copilot:gh-default";
const MONTHLY_WINDOW_SECS: u64 = 2_678_400;

pub struct CopilotProvider;

impl Default for CopilotProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CopilotProvider {
    pub fn new() -> Self {
        Self
    }
}

impl UsageProvider for CopilotProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            id: "copilot",
            display_name: "GitHub Copilot",
            icon_name: "provider-copilot",
            dashboard_url: "https://github.com/settings/copilot",
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
        let home_ref = ctx.home_dir.as_deref();

        for (idx, m) in manual.iter().enumerate() {
            let resolved_path = m.path.as_deref().map(expand_tilde);
            let source_label = resolved_path
                .as_deref()
                .map(|p| abbreviate_home(p, home_ref));
            let dedup_key = resolved_path
                .as_deref()
                .and_then(inspect_copilot_file_identity);

            out.push(AccountDescriptor {
                provider_id: "copilot".to_string(),
                id: format!("copilot:{}", slugify_id(&m.id)),
                label: m.display_label(),
                dedup_key,
                path: resolved_path,
                source_label,
                binary: m.binary.clone(),
                is_system_default: idx == 0 && !auto_discover,
                poll_mode: m.poll_mode,
                login_command: Some("gh auth login".to_string()),
                switch_command: m.switch_command.clone(),
            });
        }

        if auto_discover {
            if let Ok(env_tok) = env::var("GITHUB_COPILOT_TOKEN") {
                let trimmed = env_tok.trim();
                if !trimmed.is_empty() {
                    out.push(AccountDescriptor {
                        provider_id: "copilot".to_string(),
                        id: "copilot:default".to_string(),
                        label: "GitHub Copilot".to_string(),
                        dedup_key: Some(fingerprint_token(trimmed)),
                        path: None,
                        source_label: Some("GITHUB_COPILOT_TOKEN".to_string()),
                        binary: None,
                        is_system_default: true,
                        poll_mode: crate::config::PollMode::Active,
                        login_command: Some("gh auth login".to_string()),
                        switch_command: None,
                    });
                    return out;
                }
            }

            if let Some(cp_file) = find_copilot_config_file(home_ref, ctx.xdg_config_dir.as_deref())
            {
                let dedup_key = inspect_copilot_file_identity(&cp_file);
                out.push(AccountDescriptor {
                    provider_id: "copilot".to_string(),
                    id: "copilot:default".to_string(),
                    label: "GitHub Copilot".to_string(),
                    dedup_key,
                    source_label: Some(abbreviate_home(&cp_file, home_ref)),
                    path: Some(cp_file),
                    binary: None,
                    is_system_default: true,
                    poll_mode: crate::config::PollMode::Active,
                    login_command: Some("gh auth login".to_string()),
                    switch_command: None,
                });
                return out;
            }

            // Opportunistic `gh` CLI discovery ONLY when `~/.config/gh/hosts.yml` has a `github.com`
            // entry and no explicit Copilot config or manual account already covers it.
            if manual.is_empty() {
                if let Some(gh_hosts) = find_gh_hosts_file(home_ref, ctx.xdg_config_dir.as_deref())
                {
                    if let Ok(raw) = fs::read_to_string(&gh_hosts) {
                        let parsed = parse_gh_hosts_yml(&raw);
                        if parsed.has_github_com {
                            let dedup_key = parsed
                                .oauth_token
                                .as_deref()
                                .map(fingerprint_token)
                                .or_else(|| parsed.active_user.as_deref().map(normalize_login_key));
                            let label = parsed
                                .active_user
                                .clone()
                                .unwrap_or_else(|| "GitHub Copilot".to_string());
                            out.push(AccountDescriptor {
                                provider_id: "copilot".to_string(),
                                id: GH_AUTO_ACCOUNT_ID.to_string(),
                                label,
                                dedup_key,
                                source_label: Some(abbreviate_home(&gh_hosts, home_ref)),
                                path: Some(gh_hosts),
                                binary: None,
                                is_system_default: true,
                                poll_mode: crate::config::PollMode::Active,
                                login_command: Some("gh auth login".to_string()),
                                switch_command: None,
                            });
                        }
                    }
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
        let is_gh_opportunistic = account.id == GH_AUTO_ACCOUNT_ID;

        let resolved = match resolve_copilot_token(account, ctx) {
            Ok(tok_info) => tok_info,
            Err(err) => {
                if is_gh_opportunistic {
                    return AccountFetchOutcome::Omit;
                }
                return AccountFetchOutcome::SignedOut {
                    message: format!("{err:#}"),
                    login_command: account.login_command.clone(),
                    label: None,
                    plan_tier: None,
                };
            }
        };

        let url = ctx
            .endpoints
            .copilot_user_url
            .as_deref()
            .unwrap_or(DEFAULT_COPILOT_USER_URL);
        let auth_header = format!("token {}", resolved.token);
        let headers = [
            ("Authorization", auth_header.as_str()),
            ("Accept", "application/json"),
            ("Editor-Version", COPILOT_EDITOR_VERSION),
            ("User-Agent", COPILOT_USER_AGENT),
        ];

        match ctx.http.get(url, &headers) {
            Ok(resp) if resp.is_success() => {
                match parse_copilot_user_json(&resp.body, ctx.now, &Local) {
                    Ok(parsed) => {
                        let final_label = resolve_copilot_label(
                            account,
                            parsed.login.as_deref().or(resolved.user_hint.as_deref()),
                        );
                        AccountFetchOutcome::Success(AccountReport {
                            id: account.id.clone(),
                            label: final_label,
                            source_label: account.source_label.clone(),
                            is_system_default: account.is_system_default,
                            is_active: false,
                            plan_tier: parsed.plan_tier,
                            state: AccountState::ActiveOk,
                            is_stale: false,
                            status_note: None,
                            extra_info: parsed.extra_info,
                            error: None,
                            login_command: account.login_command.clone(),
                            updated_at_epoch: Some(ctx.now_epoch()),
                            buckets: parsed.buckets,
                            credits: None,
                            sparkline: Vec::new(),
                        })
                    }
                    Err(err) => AccountFetchOutcome::ErrorWithMeta {
                        message: format!("{err:#}"),
                        label: resolved.user_hint,
                        plan_tier: None,
                    },
                }
            }
            Ok(resp) if resp.status == 429 => AccountFetchOutcome::RateLimited {
                retry_after_secs: resp.retry_after,
            },
            Ok(resp) if is_gh_opportunistic && is_unsubscribed_copilot_response(resp.status, &resp.body) => {
                AccountFetchOutcome::Omit
            }
            Ok(resp) if resp.status == 401 || resp.status == 403 => AccountFetchOutcome::SignedOut {
                message: format!(
                    "GitHub Copilot API returned HTTP {}{}",
                    resp.status,
                    extract_short_api_message(&resp.body)
                ),
                login_command: account.login_command.clone(),
                label: resolved.user_hint,
                plan_tier: None,
            },
            Ok(resp) if resp.status == 404 => AccountFetchOutcome::ErrorWithMeta {
                message: "No active GitHub Copilot subscription found for this GitHub account"
                    .to_string(),
                label: resolved.user_hint,
                plan_tier: None,
            },
            Ok(resp) => AccountFetchOutcome::ErrorWithMeta {
                message: format!("GitHub Copilot API returned HTTP {}", resp.status),
                label: resolved.user_hint,
                plan_tier: None,
            },
            Err(err) => AccountFetchOutcome::ErrorWithMeta {
                message: format!("{err:#}"),
                label: resolved.user_hint,
                plan_tier: None,
            },
        }
    }
}

fn is_unsubscribed_copilot_response(status: u16, body: &str) -> bool {
    if status == 404 {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    if status == 403
        && (lower.contains("not subscribed")
            || lower.contains("no copilot")
            || lower.contains("access to github copilot")
            || lower.contains("copilot is not enabled")
            || lower.contains("resource not accessible"))
    {
        return true;
    }
    lower.contains("not subscribed") || lower.contains("no copilot subscription")
}

fn extract_short_api_message(body: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(msg) = val.get("message").and_then(|v| v.as_str()) {
            let trimmed = msg.trim();
            if !trimmed.is_empty() {
                return format!(" ({trimmed})");
            }
        }
    }
    String::new()
}

fn resolve_copilot_label(account: &AccountDescriptor, login: Option<&str>) -> String {
    let custom_label = account.label.trim();
    if !custom_label.is_empty()
        && custom_label != "GitHub Copilot"
        && custom_label != "default"
        && custom_label != "gh-default"
    {
        return custom_label.to_string();
    }
    if let Some(user) = login.map(str::trim).filter(|s| !s.is_empty()) {
        return user.to_string();
    }
    if custom_label.is_empty() {
        "GitHub Copilot".to_string()
    } else {
        custom_label.to_string()
    }
}

// ---------------------------------------------------------------------------
// Read-Only Token Discovery (`GITHUB_COPILOT_TOKEN` -> `account.path` -> `hosts.json`/`apps.json` -> `gh/hosts.yml`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ResolvedCopilotToken {
    pub token: String,
    pub user_hint: Option<String>,
}

fn fingerprint_token(token: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(b"copilot:");
    hasher.update(token.trim().as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(24);
    hex.push_str("tok:");
    for b in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{b:02x}");
    }
    hex
}

fn normalize_login_key(login: &str) -> String {
    format!("gh-user:{}", login.trim().to_ascii_lowercase())
}

fn inspect_copilot_file_identity(path: &Path) -> Option<String> {
    let target = resolve_copilot_candidate_path(path)?;
    let raw = fs::read_to_string(&target).ok()?;
    if let Ok(tok_info) = parse_copilot_credential_text(&raw, Some(&target)) {
        return Some(fingerprint_token(&tok_info.token));
    }
    let gh = parse_gh_hosts_yml(&raw);
    if gh.has_github_com {
        return gh
            .oauth_token
            .as_deref()
            .map(fingerprint_token)
            .or_else(|| gh.active_user.as_deref().map(normalize_login_key));
    }
    None
}

pub fn find_copilot_config_file(home: Option<&Path>, xdg_config: Option<&Path>) -> Option<PathBuf> {
    let mut roots = Vec::new();
    if let Some(xdg) = xdg_config {
        roots.push(xdg.join("github-copilot"));
    }
    if let Some(h) = home {
        let fallback = h.join(".config").join("github-copilot");
        if !roots.contains(&fallback) {
            roots.push(fallback);
        }
    }

    for dir in roots {
        for filename in ["hosts.json", "apps.json"] {
            let candidate = dir.join(filename);
            if candidate.is_file() {
                if let Ok(raw) = fs::read_to_string(&candidate) {
                    if parse_copilot_json_token(&raw).is_some() {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    None
}

pub fn find_gh_hosts_file(home: Option<&Path>, xdg_config: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(xdg) = xdg_config {
        candidates.push(xdg.join("gh").join("hosts.yml"));
    }
    if let Some(h) = home {
        let fallback = h.join(".config").join("gh").join("hosts.yml");
        if !candidates.contains(&fallback) {
            candidates.push(fallback);
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn resolve_copilot_candidate_path(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if path.is_dir() {
        for sub in ["hosts.json", "apps.json", "hosts.yml"] {
            let candidate = path.join(sub);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn resolve_copilot_token(
    account: &AccountDescriptor,
    ctx: &FetchContext,
) -> Result<ResolvedCopilotToken> {
    // (a) `GITHUB_COPILOT_TOKEN` env var (for system default if set)
    if account.is_system_default && account.path.is_none() {
        if let Ok(tok) = env::var("GITHUB_COPILOT_TOKEN") {
            let trimmed = tok.trim();
            if !trimmed.is_empty() {
                return Ok(ResolvedCopilotToken {
                    token: trimmed.to_string(),
                    user_hint: None,
                });
            }
        }
    }

    // (b) Custom `account.path` file (or discovered path)
    if let Some(ref p) = account.path {
        let target = resolve_copilot_candidate_path(p)
            .ok_or_else(|| anyhow!("Copilot credential file not found at {}", p.display()))?;
        return read_copilot_token_from_file(&target, account.binary.as_deref(), ctx.timeout);
    }

    // Precedence fallback when manual account omits `path`:
    // (a) env var -> (c) `~/.config/github-copilot/{hosts,apps}.json` -> (d) `~/.config/gh/hosts.yml`
    if let Ok(tok) = env::var("GITHUB_COPILOT_TOKEN") {
        let trimmed = tok.trim();
        if !trimmed.is_empty() {
            return Ok(ResolvedCopilotToken {
                token: trimmed.to_string(),
                user_hint: None,
            });
        }
    }

    let home_ref = ctx.home_dir.as_deref();
    let xdg_ref = ctx.xdg_config_dir.as_deref();
    if let Some(cp_file) = find_copilot_config_file(home_ref, xdg_ref) {
        if let Ok(tok) = read_copilot_token_from_file(&cp_file, account.binary.as_deref(), ctx.timeout) {
            return Ok(tok);
        }
    }

    if let Some(gh_hosts) = find_gh_hosts_file(home_ref, xdg_ref) {
        return read_copilot_token_from_file(&gh_hosts, account.binary.as_deref(), ctx.timeout);
    }

    Err(anyhow!(
        "GitHub Copilot OAuth token not found in GITHUB_COPILOT_TOKEN, ~/.config/github-copilot, or ~/.config/gh/hosts.yml"
    ))
}

fn read_copilot_token_from_file(
    file_path: &Path,
    custom_gh_binary: Option<&str>,
    timeout: Duration,
) -> Result<ResolvedCopilotToken> {
    let raw = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read {}", file_path.display()))?;

    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if ext == "yml" || ext == "yaml" {
        return resolve_from_gh_hosts_yml(&raw, file_path, custom_gh_binary, timeout);
    }

    parse_copilot_credential_text(&raw, Some(file_path))
}

/// Parses JSON (`{oauth_token}`, `hosts.json`, `apps.json`), YAML `hosts.yml`, or single-line plaintext token.
pub fn parse_copilot_credential_text(
    raw: &str,
    source_path: Option<&Path>,
) -> Result<ResolvedCopilotToken> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("Copilot credential file is empty"));
    }

    if trimmed.starts_with('{') {
        if let Some(tok) = parse_copilot_json_token(trimmed) {
            return Ok(tok);
        }
        return Err(anyhow!(
            "No github.com oauth_token found in JSON credential file{}",
            source_path
                .map(|p| format!(" {}", p.display()))
                .unwrap_or_default()
        ));
    }

    let gh = parse_gh_hosts_yml(trimmed);
    if gh.has_github_com {
        if let Some(tok) = gh.oauth_token {
            return Ok(ResolvedCopilotToken {
                token: tok,
                user_hint: gh.active_user,
            });
        }
        return Err(anyhow!("hosts.yml contains github.com without inline oauth_token"));
    }

    // Single-line plaintext token file (no spaces/colons)
    let first_non_comment = trimmed
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'));
    if let Some(line) = first_non_comment {
        if !line.contains(char::is_whitespace) && !line.contains(':') && line.len() >= 8 {
            return Ok(ResolvedCopilotToken {
                token: line.to_string(),
                user_hint: None,
            });
        }
    }

    Err(anyhow!("Unrecognized Copilot credential file format"))
}

pub fn parse_copilot_json_token(raw: &str) -> Option<ResolvedCopilotToken> {
    let val: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = val.as_object()?;

    // Direct `{ "oauth_token": "...", "user": "..." }` or `{ "token": "..." }`
    for direct_key in ["oauth_token", "token", "access_token"] {
        if let Some(tok) = obj
            .get(direct_key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let user_hint = obj
                .get("user")
                .or_else(|| obj.get("login"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            return Some(ResolvedCopilotToken {
                token: tok.to_string(),
                user_hint,
            });
        }
    }

    // `hosts.json` (`"github.com": { "oauth_token": "...", "user": "..." }`) and
    // `apps.json` (`"github.com:<client_id>": { "oauth_token": "...", "user": "..." }`)
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort_by_key(|k| {
        if k.as_str() == "github.com" {
            0usize
        } else if k.starts_with("github.com:") {
            1usize
        } else if k.contains("github.com") {
            2usize
        } else {
            3usize
        }
    });

    for key in keys {
        if !key.contains("github.com") {
            continue;
        }
        if let Some(entry) = obj.get(key).and_then(|v| v.as_object()) {
            if let Some(tok) = entry
                .get("oauth_token")
                .or_else(|| entry.get("token"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let user_hint = entry
                    .get("user")
                    .or_else(|| entry.get("login"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string);
                return Some(ResolvedCopilotToken {
                    token: tok.to_string(),
                    user_hint,
                });
            }
        }
    }

    None
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedGhHostsYml {
    pub has_github_com: bool,
    pub active_user: Option<String>,
    pub oauth_token: Option<String>,
}

/// Pure-Rust indentation-aware reader for `~/.config/gh/hosts.yml` (`github.com` section).
/// Supports both top-level `oauth_token:` and `gh` 2.40+ `users.<active>.oauth_token` maps,
/// as well as keyring-backed `hosts.yml` files where `oauth_token` is omitted from disk.
pub fn parse_gh_hosts_yml(raw: &str) -> ParsedGhHostsYml {
    let mut in_github_com = false;
    let mut host_indent = 0usize;
    let mut direct_token: Option<String> = None;
    let mut active_user: Option<String> = None;

    let mut in_users_map = false;
    let mut users_indent = 0usize;
    let mut current_user_key: Option<(usize, String)> = None;
    let mut user_tokens: BTreeMap<String, String> = BTreeMap::new();
    let mut user_order: Vec<String> = Vec::new();

    for raw_line in raw.lines() {
        let without_comment = strip_yaml_comment(raw_line);
        if without_comment.trim().is_empty() {
            continue;
        }
        let indent = without_comment
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count();
        let trimmed = without_comment.trim();

        if !in_github_com {
            if let Some((key, rest)) = split_yaml_key_val(trimmed) {
                if unquote_yaml(key) == "github.com" && rest.is_empty() {
                    in_github_com = true;
                    host_indent = indent;
                }
            }
            continue;
        }

        // Exited `github.com:` block when another key appears at `<= host_indent`
        if indent <= host_indent {
            break;
        }

        if in_users_map && indent <= users_indent {
            in_users_map = false;
            current_user_key = None;
        }

        let Some((raw_key, raw_val)) = split_yaml_key_val(trimmed) else {
            continue;
        };
        let key = unquote_yaml(raw_key);
        let val = unquote_yaml(raw_val);

        if !in_users_map {
            match key {
                "oauth_token" if !val.is_empty() => {
                    direct_token = Some(val.to_string());
                }
                "user" if !val.is_empty() => {
                    active_user = Some(val.to_string());
                }
                "users" if val.is_empty() => {
                    in_users_map = true;
                    users_indent = indent;
                    current_user_key = None;
                }
                _ => {}
            }
        } else {
            if let Some((u_indent, _)) = current_user_key {
                if indent <= u_indent {
                    current_user_key = None;
                }
            }

            if current_user_key.is_none() {
                if !key.is_empty() {
                    let uname = key.to_string();
                    if !user_order.contains(&uname) {
                        user_order.push(uname.clone());
                    }
                    current_user_key = Some((indent, uname));
                }
            } else if let Some((_, ref uname)) = current_user_key {
                if key == "oauth_token" && !val.is_empty() {
                    user_tokens.insert(uname.clone(), val.to_string());
                }
            }
        }
    }

    if !in_github_com {
        return ParsedGhHostsYml::default();
    }

    if active_user.is_none() {
        active_user = user_order.first().cloned();
    }

    let resolved_token = direct_token
        .or_else(|| {
            active_user
                .as_deref()
                .and_then(|u| user_tokens.get(u).cloned())
        })
        .or_else(|| {
            user_order
                .iter()
                .find_map(|u| user_tokens.get(u).cloned())
        });

    ParsedGhHostsYml {
        has_github_com: true,
        active_user,
        oauth_token: resolved_token,
    }
}

fn strip_yaml_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single
                && !in_double
                && (idx == 0 || line[..idx].ends_with(char::is_whitespace)) =>
            {
                return &line[..idx];
            }
            _ => {}
        }
    }
    line
}

fn split_yaml_key_val(trimmed: &str) -> Option<(&str, &str)> {
    let colon_idx = trimmed.find(':')?;
    let key = trimmed[..colon_idx].trim();
    let val = trimmed[colon_idx + 1..].trim();
    Some((key, val))
}

fn unquote_yaml(s: &str) -> &str {
    let t = s.trim();
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
    {
        &t[1..t.len() - 1]
    } else {
        t
    }
}

fn resolve_from_gh_hosts_yml(
    raw: &str,
    file_path: &Path,
    custom_gh_binary: Option<&str>,
    timeout: Duration,
) -> Result<ResolvedCopilotToken> {
    let parsed = parse_gh_hosts_yml(raw);
    if !parsed.has_github_com {
        return Err(anyhow!(
            "No github.com entry found in {}",
            file_path.display()
        ));
    }

    if let Some(tok) = parsed.oauth_token {
        return Ok(ResolvedCopilotToken {
            token: tok,
            user_hint: parsed.active_user,
        });
    }

    // Non-interactive `gh auth token -h github.com` read-only fallback ONLY when `hosts.yml`
    // exists on disk with `github.com` and no inline `oauth_token` (OS keyring storage).
    let gh_bin = resolve_binary_for("gh", custom_gh_binary);
    let spec = CommandSpec::new(gh_bin, timeout.min(Duration::from_secs(5)))
        .args(["auth", "token", "-h", "github.com"])
        .env("GH_PROMPT_DISABLED", "1");

    let out = proc_exec::run_command(&spec)
        .context("Failed to run non-interactive `gh auth token -h github.com`")?;
    if !out.status.success() {
        return Err(anyhow!(
            "`gh auth token -h github.com` exited with status {:?}",
            out.status.code()
        ));
    }

    let tok = out.stdout.trim();
    if tok.is_empty() {
        return Err(anyhow!("`gh auth token -h github.com` returned empty output"));
    }

    Ok(ResolvedCopilotToken {
        token: tok.to_string(),
        user_hint: parsed.active_user,
    })
}

// ---------------------------------------------------------------------------
// `/copilot_internal/user` JSON Response Parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ParsedCopilotUsage {
    pub login: Option<String>,
    pub plan_tier: Option<String>,
    pub extra_info: Option<String>,
    pub buckets: Vec<UsageBucket>,
}

#[derive(Debug, Deserialize)]
struct CopilotUserResponse {
    #[serde(default)]
    login: Option<String>,
    #[serde(default)]
    copilot_plan: Option<String>,
    #[serde(default)]
    access_type_sku: Option<String>,
    #[serde(default)]
    quota_reset_date_utc: Option<String>,
    #[serde(default)]
    quota_reset_date: Option<String>,
    #[serde(default)]
    limited_user_reset_date: Option<String>,
    #[serde(default)]
    quota_snapshots: Option<CopilotQuotaSnapshots>,
    #[serde(default)]
    limited_user_quotas: Option<CopilotLegacyQuotas>,
    #[serde(default)]
    monthly_quotas: Option<CopilotLegacyQuotas>,
}

#[derive(Debug, Deserialize)]
struct CopilotQuotaSnapshots {
    #[serde(default)]
    premium_interactions: Option<CopilotQuotaSnapshot>,
    #[serde(default)]
    chat: Option<CopilotQuotaSnapshot>,
    #[serde(default)]
    completions: Option<CopilotQuotaSnapshot>,
}

#[derive(Debug, Deserialize)]
struct CopilotQuotaSnapshot {
    #[serde(default)]
    entitlement: Option<f64>,
    #[serde(default)]
    remaining: Option<f64>,
    #[serde(default)]
    quota_remaining: Option<f64>,
    #[serde(default)]
    percent_remaining: Option<f64>,
    #[serde(default)]
    unlimited: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CopilotLegacyQuotas {
    #[serde(default)]
    chat: Option<f64>,
    #[serde(default)]
    completions: Option<f64>,
}

pub fn map_copilot_plan_badge(plan: Option<&str>, sku: Option<&str>) -> Option<String> {
    if let Some(raw) = plan.map(str::trim).filter(|s| !s.is_empty()) {
        let lower = raw.to_ascii_lowercase();
        return Some(match lower.as_str() {
            "individual" => "Pro".to_string(),
            "individual_pro" => "Pro+".to_string(),
            "business" => "Business".to_string(),
            "enterprise" => "Enterprise".to_string(),
            "free" | "copilot_free" | "free_limited" => "Free".to_string(),
            _ => humanize_plan(raw),
        });
    }

    if let Some(raw_sku) = sku.map(str::trim).filter(|s| !s.is_empty()) {
        let lower = raw_sku.to_ascii_lowercase();
        if lower.contains("enterprise") {
            return Some("Enterprise".to_string());
        }
        if lower.contains("business") {
            return Some("Business".to_string());
        }
        if lower.contains("pro_plus") || lower.contains("individual_pro") {
            return Some("Pro+".to_string());
        }
        if lower.contains("free") {
            return Some("Free".to_string());
        }
        if lower.contains("individual") || lower.contains("pro") {
            return Some("Pro".to_string());
        }
    }

    None
}

fn humanize_plan(raw: &str) -> String {
    raw.split(['_', '-', ' '])
        .filter(|s| !s.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut out = first.to_uppercase().collect::<String>();
                    out.push_str(&chars.as_str().to_ascii_lowercase());
                    out
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_copilot_reset<T: TimeZone>(
    reset_utc: Option<&str>,
    reset_date: Option<&str>,
    legacy_reset: Option<&str>,
    now: DateTime<Utc>,
    tz: &T,
) -> Option<(i64, String)>
where
    T::Offset: std::fmt::Display,
{
    for candidate in [reset_utc, reset_date, legacy_reset].into_iter().flatten() {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(norm) = reset::normalize_reset(trimmed, now, tz) {
            return Some(norm);
        }
        // Handle date-only strings like `"2026-12-01"` returned by `quota_reset_date`
        if trimmed.len() == 10
            && trimmed.as_bytes().get(4) == Some(&b'-')
            && trimmed.as_bytes().get(7) == Some(&b'-')
        {
            let iso_midnight = format!("{trimmed}T00:00:00Z");
            if let Some(norm) = reset::normalize_reset(&iso_midnight, now, tz) {
                return Some(norm);
            }
        }
    }
    None
}

fn compute_snapshot_used_pct(snap: &CopilotQuotaSnapshot) -> f64 {
    if snap.unlimited == Some(true) {
        return 0.0;
    }
    if let Some(rem_pct) = snap.percent_remaining.filter(|v| v.is_finite()) {
        return (100.0 - rem_pct).clamp(0.0, 100.0);
    }
    let rem_opt = snap.remaining.or(snap.quota_remaining);
    if let (Some(ent), Some(rem)) = (snap.entitlement, rem_opt) {
        if ent.is_finite() && rem.is_finite() && ent > 0.0 {
            return (((ent - rem) / ent) * 100.0).clamp(0.0, 100.0);
        }
    }
    0.0
}

pub fn parse_copilot_user_json<T: TimeZone>(
    raw: &str,
    now: DateTime<Utc>,
    tz: &T,
) -> Result<ParsedCopilotUsage>
where
    T::Offset: std::fmt::Display,
{
    let parsed: CopilotUserResponse =
        serde_json::from_str(raw).context("Failed to parse Copilot user JSON")?;

    let login = parsed
        .login
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let plan_tier = map_copilot_plan_badge(
        parsed.copilot_plan.as_deref(),
        parsed.access_type_sku.as_deref(),
    );
    let norm_reset = normalize_copilot_reset(
        parsed.quota_reset_date_utc.as_deref(),
        parsed.quota_reset_date.as_deref(),
        parsed.limited_user_reset_date.as_deref(),
        now,
        tz,
    );

    let mut buckets: Vec<UsageBucket> = Vec::new();
    let mut account_extra_info: Option<String> = None;

    if let Some(ref snaps) = parsed.quota_snapshots {
        if let Some(ref prem) = snaps.premium_interactions {
            if prem.unlimited == Some(true) {
                let note = "Unlimited premium requests".to_string();
                account_extra_info = Some(note.clone());
                buckets.push(
                    UsageBucket::new(
                        "premium",
                        "Premium Requests",
                        "31d",
                        BucketLayout::InlinePair,
                        true,
                        0.0,
                        norm_reset.clone(),
                    )
                    .with_window_seconds(Some(MONTHLY_WINDOW_SECS))
                    .with_extra_info(note),
                );
            } else {
                let used_pct = compute_snapshot_used_pct(prem);
                let rem_opt = prem.remaining.or(prem.quota_remaining);
                let mut bucket = UsageBucket::new(
                    "premium",
                    "Premium Requests",
                    "31d",
                    BucketLayout::InlinePair,
                    true,
                    used_pct,
                    norm_reset.clone(),
                )
                .with_window_seconds(Some(MONTHLY_WINDOW_SECS));

                if let (Some(ent), Some(rem)) = (prem.entitlement, rem_opt) {
                    if ent.is_finite() && rem.is_finite() && ent >= 0.0 {
                        let ent_int = ent.round().max(0.0) as u64;
                        let used_int = (ent - rem).round().max(0.0) as u64;
                        let info = format!("{used_int} / {ent_int} premium requests used");
                        account_extra_info = Some(info.clone());
                        bucket = bucket.with_extra_info(info);
                    }
                }
                buckets.push(bucket);
            }
        }

        if let Some(ref chat) = snaps.chat {
            if chat.unlimited == Some(false) {
                let used_pct = compute_snapshot_used_pct(chat);
                let is_primary = buckets.is_empty();
                buckets.push(
                    UsageBucket::new(
                        "chat",
                        "Chat",
                        "31d",
                        BucketLayout::InlinePair,
                        is_primary,
                        used_pct,
                        norm_reset.clone(),
                    )
                    .with_window_seconds(Some(MONTHLY_WINDOW_SECS)),
                );
            }
        }

        if let Some(ref comp) = snaps.completions {
            if comp.unlimited == Some(false) {
                let used_pct = compute_snapshot_used_pct(comp);
                let is_primary = buckets.is_empty();
                buckets.push(
                    UsageBucket::new(
                        "completions",
                        "Completions",
                        "31d",
                        BucketLayout::InlinePair,
                        is_primary,
                        used_pct,
                        norm_reset.clone(),
                    )
                    .with_window_seconds(Some(MONTHLY_WINDOW_SECS)),
                );
            }
        }
    }

    // Legacy Copilot Free fallback (`limited_user_quotas` + `monthly_quotas`)
    if buckets.is_empty() {
        if let (Some(ref rem_q), Some(ref limit_q)) =
            (&parsed.limited_user_quotas, &parsed.monthly_quotas)
        {
            if let (Some(rem_chat), Some(tot_chat)) = (rem_q.chat, limit_q.chat) {
                if tot_chat > 0.0 {
                    let used_pct = (((tot_chat - rem_chat) / tot_chat) * 100.0).clamp(0.0, 100.0);
                    let used_int = (tot_chat - rem_chat).round().max(0.0) as u64;
                    let tot_int = tot_chat.round().max(0.0) as u64;
                    let info = format!("{used_int} / {tot_int} chat messages used");
                    account_extra_info = Some(info.clone());
                    buckets.push(
                        UsageBucket::new(
                            "chat",
                            "Chat",
                            "31d",
                            BucketLayout::InlinePair,
                            true,
                            used_pct,
                            norm_reset.clone(),
                        )
                        .with_window_seconds(Some(MONTHLY_WINDOW_SECS))
                        .with_extra_info(info),
                    );
                }
            }
            if let (Some(rem_comp), Some(tot_comp)) = (rem_q.completions, limit_q.completions) {
                if tot_comp > 0.0 {
                    let used_pct = (((tot_comp - rem_comp) / tot_comp) * 100.0).clamp(0.0, 100.0);
                    let is_primary = buckets.is_empty();
                    buckets.push(
                        UsageBucket::new(
                            "completions",
                            "Completions",
                            "31d",
                            BucketLayout::InlinePair,
                            is_primary,
                            used_pct,
                            norm_reset.clone(),
                        )
                        .with_window_seconds(Some(MONTHLY_WINDOW_SECS)),
                    );
                }
            }
        }
    }

    // Business/Enterprise plans where all snapshots are unlimited and `premium_interactions` is omitted
    if buckets.is_empty() && (plan_tier.is_some() || parsed.quota_snapshots.is_some()) {
        let info = "Unlimited premium requests".to_string();
        account_extra_info = Some(info.clone());
        buckets.push(
            UsageBucket::new(
                "premium",
                "Premium Requests",
                "31d",
                BucketLayout::InlinePair,
                true,
                0.0,
                norm_reset,
            )
            .with_window_seconds(Some(MONTHLY_WINDOW_SECS))
            .with_extra_info(info),
        );
    }

    if buckets.is_empty() {
        return Err(anyhow!(
            "No Copilot quota snapshots or plan details present in response"
        ));
    }

    Ok(ParsedCopilotUsage {
        login,
        plan_tier,
        extra_info: account_extra_info,
        buckets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn test_parse_copilot_hosts_and_apps_json() {
        let hosts_json = r#"{
            "github.com": {
                "user": "octodev",
                "oauth_token": "mock_ghu_hosts_token_123"
            }
        }"#;
        let resolved = parse_copilot_json_token(hosts_json).expect("hosts.json");
        assert_eq!(resolved.token, "mock_ghu_hosts_token_123");
        assert_eq!(resolved.user_hint.as_deref(), Some("octodev"));

        let apps_json = r#"{
            "github.com:Iv1.b507a08c87ecfe98": {
                "user": "appsuser",
                "oauth_token": "mock_ghu_apps_token_456"
            }
        }"#;
        let resolved_apps = parse_copilot_json_token(apps_json).expect("apps.json");
        assert_eq!(resolved_apps.token, "mock_ghu_apps_token_456");
        assert_eq!(resolved_apps.user_hint.as_deref(), Some("appsuser"));
    }

    #[test]
    fn test_parse_gh_hosts_yml_inline_multiuser_and_keyring() {
        let classic = "\
github.com:
    user: devone
    oauth_token: mock_gho_classic_111
    git_protocol: https
";
        let p1 = parse_gh_hosts_yml(classic);
        assert!(p1.has_github_com);
        assert_eq!(p1.active_user.as_deref(), Some("devone"));
        assert_eq!(p1.oauth_token.as_deref(), Some("mock_gho_classic_111"));

        let multi = "\
github.com:
    git_protocol: https
    users:
        personal:
            oauth_token: mock_gho_personal_111
        workcorp:
            oauth_token: mock_gho_work_222
    user: workcorp
enterprise.internal:
    user: other
    oauth_token: should_not_match
";
        let p2 = parse_gh_hosts_yml(multi);
        assert!(p2.has_github_com);
        assert_eq!(p2.active_user.as_deref(), Some("workcorp"));
        assert_eq!(p2.oauth_token.as_deref(), Some("mock_gho_work_222"));

        let keyring_only = "\
github.com:
    git_protocol: https
    users:
        keyringuser:
    user: keyringuser
";
        let p3 = parse_gh_hosts_yml(keyring_only);
        assert!(p3.has_github_com);
        assert_eq!(p3.active_user.as_deref(), Some("keyringuser"));
        assert!(p3.oauth_token.is_none());
    }

    #[test]
    fn test_parse_copilot_user_pro_metered_and_unlimited_snapshots() {
        let tz = FixedOffset::east_opt(0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 16, 0, 0).unwrap();

        let pro_json = r#"{
            "login": "octocat",
            "copilot_plan": "individual",
            "quota_reset_date_utc": "2026-12-01T00:00:00.000Z",
            "quota_snapshots": {
                "premium_interactions": {
                    "entitlement": 300,
                    "remaining": 258,
                    "percent_remaining": 86.0,
                    "unlimited": false
                },
                "chat": {
                    "unlimited": true
                },
                "completions": {
                    "unlimited": true
                }
            }
        }"#;

        let parsed = parse_copilot_user_json(pro_json, now, &tz).expect("parse pro");
        assert_eq!(parsed.login.as_deref(), Some("octocat"));
        assert_eq!(parsed.plan_tier.as_deref(), Some("Pro"));
        assert_eq!(
            parsed.extra_info.as_deref(),
            Some("42 / 300 premium requests used")
        );
        assert_eq!(parsed.buckets.len(), 1);
        assert_eq!(parsed.buckets[0].id, "premium");
        assert_eq!(parsed.buckets[0].label, "Premium Requests");
        assert_eq!(parsed.buckets[0].short_label, "31d");
        assert_eq!(parsed.buckets[0].window_seconds, Some(2_678_400));
        assert_eq!(parsed.buckets[0].used_percent, 14);
        assert_eq!(parsed.buckets[0].remaining_percent, 86);
        assert_eq!(
            parsed.buckets[0].extra_info.as_deref(),
            Some("42 / 300 premium requests used")
        );
        assert!(parsed.buckets[0].resets_at_epoch.is_some());

        // Unlimited `premium_interactions` + date-only `quota_reset_date`
        let ent_json = r#"{
            "login": "corpdev",
            "copilot_plan": "enterprise",
            "quota_reset_date": "2026-12-01",
            "quota_snapshots": {
                "premium_interactions": {
                    "unlimited": true
                }
            }
        }"#;
        let ent = parse_copilot_user_json(ent_json, now, &tz).expect("parse enterprise");
        assert_eq!(ent.plan_tier.as_deref(), Some("Enterprise"));
        assert_eq!(ent.buckets[0].used_percent, 0);
        assert_eq!(
            ent.extra_info.as_deref(),
            Some("Unlimited premium requests")
        );
        assert!(ent.buckets[0].resets_at_epoch.is_some());

        // Copilot Free with metered chat & completions
        let free_json = r#"{
            "login": "freeuser",
            "copilot_plan": "free",
            "quota_reset_date_utc": "2026-12-01T00:00:00Z",
            "quota_snapshots": {
                "premium_interactions": {
                    "entitlement": 50,
                    "remaining": 35,
                    "percent_remaining": 70.0,
                    "unlimited": false
                },
                "chat": {
                    "entitlement": 500,
                    "remaining": 400,
                    "percent_remaining": 80.0,
                    "unlimited": false
                },
                "completions": {
                    "entitlement": 2000,
                    "remaining": 1500,
                    "percent_remaining": 75.0,
                    "unlimited": false
                }
            }
        }"#;
        let free = parse_copilot_user_json(free_json, now, &tz).expect("parse free");
        assert_eq!(free.plan_tier.as_deref(), Some("Free"));
        assert_eq!(free.buckets.len(), 3);
        assert_eq!(free.buckets[0].id, "premium");
        assert_eq!(free.buckets[0].used_percent, 30);
        assert_eq!(free.buckets[1].id, "chat");
        assert_eq!(free.buckets[1].used_percent, 20);
        assert_eq!(free.buckets[2].id, "completions");
        assert_eq!(free.buckets[2].used_percent, 25);
    }

    #[test]
    fn test_smart_noise_free_gh_discovery_omits_404_but_keeps_manual_error() {
        let tmp = std::env::temp_dir().join(format!(
            "gas-copilot-gh404-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = tmp.join("home");
        let xdg = home.join(".config");
        fs::create_dir_all(xdg.join("gh")).unwrap();
        fs::write(
            xdg.join("gh/hosts.yml"),
            "github.com:\n    user: gitonlyuser\n    oauth_token: mock_gh_token_abc12345\n",
        )
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let body = r#"{"message":"Not Found"}"#;
                let resp = format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let ctx = FetchContext {
            http: crate::http::HttpClient::new(Duration::from_secs(5)),
            timeout: Duration::from_secs(5),
            now: Utc.with_ymd_and_hms(2026, 11, 11, 16, 0, 0).unwrap(),
            home_dir: Some(home.clone()),
            xdg_config_dir: Some(xdg.clone()),
            proc_root: tmp.join("proc"),
            endpoints: crate::http::EndpointOverrides {
                copilot_user_url: Some(format!("http://{addr}/copilot_internal/user")),
                ..Default::default()
            },
            env_overrides: Some(std::collections::BTreeMap::new()),
        };

        let provider = CopilotProvider::new();

        // 1. Auto-discovered solely via `gh/hosts.yml` + 404 -> Omit!
        let auto_accts = provider.discover_accounts(&[], true, &ctx);
        assert_eq!(auto_accts.len(), 1);
        assert_eq!(auto_accts[0].id, GH_AUTO_ACCOUNT_ID);
        let auto_outcome = provider.fetch_account(&auto_accts[0], None, &ctx);
        assert!(matches!(auto_outcome, AccountFetchOutcome::Omit));

        // 2. Explicitly configured manual account + 404 -> ErrorWithMeta (not silently swallowed!)
        let manual_cfg = vec![ManualAccountConfig {
            id: "work".to_string(),
            label: Some("Work GitHub".to_string()),
            path: Some(xdg.join("gh/hosts.yml").to_string_lossy().into_owned()),
            binary: None,
            poll_mode: crate::config::PollMode::Active,
            switch_command: None,
        }];
        let man_accts = provider.discover_accounts(&manual_cfg, false, &ctx);
        assert_eq!(man_accts.len(), 1);
        let man_outcome = provider.fetch_account(&man_accts[0], None, &ctx);
        assert!(matches!(man_outcome, AccountFetchOutcome::ErrorWithMeta { .. }));

        server.join().unwrap();
        let _ = fs::remove_dir_all(&tmp);
    }
}
