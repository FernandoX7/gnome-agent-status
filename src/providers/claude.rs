//! Anthropic Claude Code (`claude`) usage provider:
//! - Tier 1A: Passive `statusLine` JSON hook ingestion (`ingest-hook claude`, zero network I/O)
//! - Tier 1B: Direct OAuth Usage API (`GET https://api.anthropic.com/api/oauth/usage` with `claude-code/<ver>` UA)
//! - Tier 2: Multi-bucket `CLAUDE_CONFIG_DIR=<dir> claude -p /usage` CLI fallback (SSO / setup-token / safe token refresh)

use crate::cache::{
    acquire_file_lock, apply_report_time_decay, atomic_write_0600, synthesize_report_from_cache,
    CacheStore, CachedAccountState,
};
use crate::config::{
    abbreviate_home, discover_provider_locations, expand_tilde_with_home, resolve_binary_for,
    slugify_id, Config, ManualAccountConfig,
};
use crate::model::{
    AccountReport, AccountState, BucketLayout, CreditSummary, StatusReport, UsageBucket,
};
use crate::parser::strip_ansi;
use crate::proc_exec::{run_command, CommandSpec};
use crate::providers::{
    AccountDescriptor, AccountFetchOutcome, FetchContext, ProviderMeta, UsageProvider,
};
use crate::reset;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local, TimeZone, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

const DEFAULT_CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CLAUDE_OAUTH_REFRESH_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLAUDE_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const FALLBACK_CLAUDE_UA_VERSION: &str = "2.1.76";

pub struct ClaudeProvider;

impl Default for ClaudeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeProvider {
    pub fn new() -> Self {
        Self
    }
}

impl UsageProvider for ClaudeProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            id: "claude",
            display_name: "Claude",
            icon_name: "provider-claude",
            dashboard_url: "https://claude.ai/settings/usage",
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
        let default_claude_dir = home.map(|h| h.join(".claude"));

        for m in manual {
            let expanded = m
                .path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|p| expand_tilde_with_home(p, home));

            let is_sys_default = match (&expanded, &default_claude_dir) {
                (Some(p), Some(def)) => p == def,
                (None, _) => true,
                _ => false,
            };

            let identity = expanded
                .as_deref()
                .and_then(|p| read_claude_identity(p, home));
            let source_label = expanded.as_deref().map(|p| abbreviate_home(p, home));
            let login_cmd = build_claude_login_command(
                expanded.as_deref(),
                source_label.as_deref(),
                is_sys_default,
            );

            let label = if m.label.as_deref().is_some_and(|l| !l.trim().is_empty()) {
                m.display_label()
            } else {
                identity
                    .as_ref()
                    .and_then(|i| i.email.clone().or_else(|| i.display_name.clone()))
                    .unwrap_or_else(|| m.display_label())
            };

            let dedup_key = identity
                .as_ref()
                .and_then(|i| i.account_uuid.clone().or_else(|| i.email.clone()));

            out.push(AccountDescriptor {
                provider_id: "claude".to_string(),
                id: format!("claude:{}", m.id),
                label,
                dedup_key,
                path: expanded,
                source_label,
                binary: m.binary.clone(),
                is_system_default: is_sys_default,
                poll_mode: m.poll_mode,
                login_command: Some(login_cmd),
                switch_command: m.switch_command.clone(),
            });
        }

        if auto_discover {
            for loc in discover_provider_locations("claude", home, ctx.xdg_config_dir.as_deref()) {
                let identity = read_claude_identity(&loc.path, home);
                let email = identity.as_ref().and_then(|i| i.email.clone());
                let acct_id = if let Some(ref e) = email {
                    format!("claude:{}", e.to_ascii_lowercase())
                } else {
                    format!("claude:{}", loc.suggested_id)
                };
                let label = email
                    .clone()
                    .or_else(|| identity.as_ref().and_then(|i| i.display_name.clone()))
                    .unwrap_or_else(|| {
                        if loc.is_system_default {
                            "Claude".to_string()
                        } else {
                            loc.suggested_id.clone()
                        }
                    });
                let dedup_key = identity
                    .and_then(|i| i.account_uuid.or(i.email))
                    .or_else(|| Some(loc.path.to_string_lossy().into_owned()));
                let login_cmd = build_claude_login_command(
                    Some(&loc.path),
                    Some(&loc.source_label),
                    loc.is_system_default,
                );

                out.push(AccountDescriptor {
                    provider_id: "claude".to_string(),
                    id: acct_id,
                    label,
                    dedup_key,
                    path: Some(loc.path),
                    source_label: Some(loc.source_label),
                    binary: None,
                    is_system_default: loc.is_system_default,
                    poll_mode: loc.poll_mode,
                    login_command: Some(login_cmd),
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
        let home = ctx.home_dir.as_deref();
        let profile_path = account
            .path
            .clone()
            .or_else(|| home.map(|h| h.join(".claude")));

        let identity = profile_path
            .as_deref()
            .and_then(|p| read_claude_identity(p, home));
        let creds = profile_path
            .as_deref()
            .and_then(read_claude_credentials_from_path);

        let plan_tier = creds.as_ref().and_then(extract_claude_plan_tier);
        let resolved_label = if account.label == "Claude" || account.label == "default" {
            identity
                .as_ref()
                .and_then(|i| i.email.clone().or_else(|| i.display_name.clone()))
                .unwrap_or_else(|| account.label.clone())
        } else {
            account.label.clone()
        };

        // Tier 1B: Direct OAuth Usage API when a valid `claudeAiOauth` token is present
        if let Some(mut oauth) = creds {
            let now_ms = ctx.now.timestamp_millis();
            let mut token_expired = oauth
                .expires_at_ms()
                .is_some_and(|exp_ms| now_ms >= exp_ms.saturating_sub(30_000));

            if token_expired {
                // Re-read from disk in case an active `claude` CLI process already rotated it!
                if let Some(reloaded) = profile_path
                    .as_deref()
                    .and_then(read_claude_credentials_from_path)
                {
                    oauth = reloaded;
                    token_expired = oauth
                        .expires_at_ms()
                        .is_some_and(|exp_ms| now_ms >= exp_ms.saturating_sub(30_000));
                }
            }

            // If still expired, prefer CLI fallback if `claude` binary exists (avoids rotation races);
            // otherwise attempt atomic locked HTTP token refresh if we aren't running against a mock URL.
            if token_expired {
                let cli_bin = resolve_binary_for("claude", account.binary.as_deref());
                if cli_bin.is_file() {
                    return fetch_via_claude_cli(
                        account,
                        profile_path.as_deref(),
                        &resolved_label,
                        plan_tier,
                        ctx,
                    );
                }
                if let (Some(ref p), Some(ref rt)) = (&profile_path, &oauth.refresh_token) {
                    if !rt.trim().is_empty() && ctx.endpoints.claude_usage_url.is_none() {
                        if let Ok(refreshed) = try_refresh_claude_oauth(p, rt, ctx) {
                            oauth = refreshed;
                            token_expired = false;
                        }
                    }
                }
            }

            if !token_expired && !oauth.access_token.trim().is_empty() {
                let usage_url = ctx
                    .endpoints
                    .claude_usage_url
                    .as_deref()
                    .unwrap_or(DEFAULT_CLAUDE_USAGE_URL);
                let user_agent = claude_user_agent(account.binary.as_deref());
                let auth_header = format!("Bearer {}", oauth.access_token.trim());
                let headers = [
                    ("Authorization", auth_header.as_str()),
                    ("anthropic-beta", "oauth-2025-04-20"),
                    ("User-Agent", user_agent.as_str()),
                    ("Accept", "application/json"),
                ];

                match ctx.http.get(usage_url, &headers) {
                    Ok(resp) if resp.is_success() => {
                        match parse_claude_oauth_usage_json(&resp.body, ctx.now, &Local) {
                            Ok((buckets, credits)) => {
                                return AccountFetchOutcome::Success(AccountReport {
                                    id: account.id.clone(),
                                    label: resolved_label,
                                    source_label: account.source_label.clone(),
                                    is_system_default: account.is_system_default,
                                    is_active: false,
                                    plan_tier,
                                    state: AccountState::ActiveOk,
                                    is_stale: false,
                                    status_note: None,
                                    error: None,
                                    login_command: account.login_command.clone(),
                                    updated_at_epoch: Some(ctx.now_epoch()),
                                    buckets,
                                    credits,
                                });
                            }
                            Err(parse_err) => {
                                return AccountFetchOutcome::Error {
                                    message: format!("{:#}", parse_err),
                                };
                            }
                        }
                    }
                    Ok(resp) if resp.status == 429 => {
                        return AccountFetchOutcome::RateLimited {
                            retry_after_secs: resp.retry_after,
                        };
                    }
                    Ok(resp) if resp.status == 401 || resp.status == 403 => {
                        // Try CLI fallback if installed (e.g., setup-token / SSO), else SignedOut
                        let cli_bin = resolve_binary_for("claude", account.binary.as_deref());
                        if cli_bin.is_file() {
                            return fetch_via_claude_cli(
                                account,
                                profile_path.as_deref(),
                                &resolved_label,
                                plan_tier,
                                ctx,
                            );
                        }
                        return AccountFetchOutcome::SignedOut {
                            message: format!("Anthropic OAuth returned HTTP {}", resp.status),
                            login_command: account.login_command.clone(),
                            label: Some(resolved_label),
                            plan_tier,
                        };
                    }
                    Ok(resp) => {
                        return AccountFetchOutcome::Error {
                            message: format!("Anthropic OAuth returned HTTP {}", resp.status),
                        };
                    }
                    Err(net_err) => {
                        let cli_bin = resolve_binary_for("claude", account.binary.as_deref());
                        if cli_bin.is_file() {
                            return fetch_via_claude_cli(
                                account,
                                profile_path.as_deref(),
                                &resolved_label,
                                plan_tier,
                                ctx,
                            );
                        }
                        return AccountFetchOutcome::Error {
                            message: format!("{:#}", net_err),
                        };
                    }
                }
            }
        }

        // Tier 2: CLI fallback (`claude -p /usage`) for corporate SSO / `setup-token` or missing `.credentials.json`
        let cli_bin = resolve_binary_for("claude", account.binary.as_deref());
        if cli_bin.is_file() || account.binary.is_some() {
            return fetch_via_claude_cli(
                account,
                profile_path.as_deref(),
                &resolved_label,
                plan_tier,
                ctx,
            );
        }

        AccountFetchOutcome::SignedOut {
            message: "Claude credentials not found".to_string(),
            login_command: account.login_command.clone(),
            label: Some(resolved_label),
            plan_tier,
        }
    }
}

// ---------------------------------------------------------------------------
// Credential & Identity Discovery Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ClaudeIdentity {
    account_uuid: Option<String>,
    email: Option<String>,
    display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClaudeCredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeOauthBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClaudeOauthBlock {
    #[serde(rename = "accessToken", default)]
    access_token: String,
    #[serde(rename = "refreshToken", default)]
    refresh_token: Option<String>,
    #[serde(rename = "expiresAt", default)]
    expires_at: Option<i64>,
    #[serde(rename = "subscriptionType", default)]
    subscription_type: Option<String>,
    #[serde(rename = "rateLimitTier", default)]
    rate_limit_tier: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl ClaudeOauthBlock {
    fn expires_at_ms(&self) -> Option<i64> {
        self.expires_at.map(|ts| {
            if ts > 0 && ts < 1_000_000_000_000 {
                ts * 1000
            } else {
                ts
            }
        })
    }
}

fn resolve_credentials_file_path(path: &Path) -> PathBuf {
    if path.is_file() {
        path.to_path_buf()
    } else {
        path.join(".credentials.json")
    }
}

fn read_claude_credentials_from_path(path: &Path) -> Option<ClaudeOauthBlock> {
    let cred_file = resolve_credentials_file_path(path);
    let raw = fs::read_to_string(cred_file).ok()?;
    let parsed: ClaudeCredentialsFile = serde_json::from_str(&raw).ok()?;
    parsed.claude_ai_oauth
}

fn read_claude_identity(path: &Path, home: Option<&Path>) -> Option<ClaudeIdentity> {
    let dir = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };

    let mut candidates = vec![dir.join(".claude.json")];
    if let Some(h) = home {
        if dir == h.join(".claude") {
            candidates.push(h.join(".claude.json"));
        }
    }

    for candidate in candidates {
        let Ok(raw) = fs::read_to_string(&candidate) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let Some(acct) = val.get("oauthAccount") else {
            continue;
        };
        let account_uuid = acct
            .get("accountUuid")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let email = acct
            .get("emailAddress")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let display_name = acct
            .get("displayName")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        if account_uuid.is_some() || email.is_some() || display_name.is_some() {
            return Some(ClaudeIdentity {
                account_uuid,
                email,
                display_name,
            });
        }
    }
    None
}

fn extract_claude_plan_tier(oauth: &ClaudeOauthBlock) -> Option<String> {
    let tier = oauth
        .rate_limit_tier
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let sub = oauth
        .subscription_type
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    if tier.contains("20x") {
        Some("Max 20x".to_string())
    } else if tier.contains("5x") {
        Some("Max 5x".to_string())
    } else if sub == "max" || tier.contains("max") {
        Some("Max".to_string())
    } else if sub == "pro" || tier.contains("pro") {
        Some("Pro".to_string())
    } else if sub == "team" || tier.contains("team") {
        Some("Team".to_string())
    } else if sub == "enterprise" || tier.contains("enterprise") {
        Some("Enterprise".to_string())
    } else {
        None
    }
}

fn build_claude_login_command(
    path: Option<&Path>,
    source_label: Option<&str>,
    is_system_default: bool,
) -> String {
    if is_system_default {
        return "claude auth login".to_string();
    }
    let dir_str = source_label
        .map(String::from)
        .or_else(|| path.map(|p| p.to_string_lossy().into_owned()));
    match dir_str {
        Some(d) if !d.is_empty() => {
            if d.contains(' ') {
                format!("CLAUDE_CONFIG_DIR=\"{d}\" claude auth login")
            } else {
                format!("CLAUDE_CONFIG_DIR={d} claude auth login")
            }
        }
        _ => "claude auth login".to_string(),
    }
}

/// Detects `claude --version` once per process so `User-Agent: claude-code/<version>`
/// matches the installed CLI and never triggers Anthropic/Cloudflare missing-UA 429 throttles.
fn claude_user_agent(custom_binary: Option<&str>) -> String {
    static DETECTED_UA: OnceLock<String> = OnceLock::new();
    if custom_binary.is_none() {
        return DETECTED_UA
            .get_or_init(|| detect_claude_ua(None))
            .clone();
    }
    detect_claude_ua(custom_binary)
}

fn detect_claude_ua(custom_binary: Option<&str>) -> String {
    let bin = resolve_binary_for("claude", custom_binary);
    if bin.is_file() {
        let spec = CommandSpec::new(bin, Duration::from_millis(1500)).args(["--version"]);
        if let Ok(out) = run_command(&spec) {
            if let Some(ver) = extract_semver(&out.stdout) {
                return format!("claude-code/{ver}");
            }
        }
    }
    format!("claude-code/{FALLBACK_CLAUDE_UA_VERSION}")
}

fn extract_semver(raw: &str) -> Option<String> {
    let re = Regex::new(r"(\d+\.\d+\.\d+)").ok()?;
    re.captures(raw)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn try_refresh_claude_oauth(
    profile_path: &Path,
    refresh_token: &str,
    ctx: &FetchContext,
) -> Result<ClaudeOauthBlock> {
    let cred_file = resolve_credentials_file_path(profile_path);
    let lock_path = cred_file.with_extension("json.lock");
    let _guard = acquire_file_lock(&lock_path)?;

    // Re-read under flock before consuming single-use refresh token!
    if let Some(reloaded) = read_claude_credentials_from_path(profile_path) {
        if reloaded
            .expires_at_ms()
            .is_some_and(|exp_ms| ctx.now.timestamp_millis() < exp_ms.saturating_sub(30_000))
        {
            return Ok(reloaded);
        }
    }

    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLAUDE_OAUTH_CLIENT_ID,
    })
    .to_string();

    let ua = claude_user_agent(None);
    let resp = ctx
        .http
        .post_json(CLAUDE_OAUTH_REFRESH_URL, &[("User-Agent", ua.as_str())], &body)?;

    if !resp.is_success() {
        return Err(anyhow!("OAuth token refresh returned HTTP {}", resp.status));
    }

    let val: serde_json::Value =
        serde_json::from_str(&resp.body).context("Invalid OAuth refresh JSON")?;
    let new_access = val
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing access_token in refresh response"))?;
    let new_refresh = val
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(String::from);
    let expires_in_secs = val
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);
    let new_expires_ms = ctx.now.timestamp_millis() + expires_in_secs * 1000;

    let mut root_json: serde_json::Value = fs::read_to_string(&cred_file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    if let Some(obj) = root_json.as_object_mut() {
        let oauth_entry = obj
            .entry("claudeAiOauth")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(oauth_obj) = oauth_entry.as_object_mut() {
            oauth_obj.insert(
                "accessToken".to_string(),
                serde_json::Value::String(new_access.to_string()),
            );
            if let Some(ref rt) = new_refresh {
                oauth_obj.insert(
                    "refreshToken".to_string(),
                    serde_json::Value::String(rt.clone()),
                );
            }
            oauth_obj.insert(
                "expiresAt".to_string(),
                serde_json::Value::Number(new_expires_ms.into()),
            );
        }
    }

    let serialized = serde_json::to_vec_pretty(&root_json)?;
    atomic_write_0600(&cred_file, &serialized)?;

    read_claude_credentials_from_path(profile_path)
        .ok_or_else(|| anyhow!("Failed to reload refreshed Claude credentials"))
}

// ---------------------------------------------------------------------------
// Tier 1B: Anthropic `/api/oauth/usage` JSON Parser
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ClaudeUsageApiResponse {
    #[serde(default)]
    five_hour: Option<ClaudeApiWindow>,
    #[serde(default)]
    seven_day: Option<ClaudeApiWindow>,
    #[serde(default)]
    seven_day_sonnet: Option<ClaudeApiWindow>,
    #[serde(default)]
    seven_day_opus: Option<ClaudeApiWindow>,
    #[serde(default)]
    seven_day_cowork: Option<ClaudeApiWindow>,
    #[serde(default)]
    extra_usage: Option<ClaudeExtraUsage>,
    #[serde(default)]
    limits: Vec<ClaudeScopedLimit>,
    #[serde(flatten)]
    extra_buckets: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ClaudeScopedLimit {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    percent: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
    #[serde(default)]
    scope: Option<ClaudeLimitScope>,
}

#[derive(Debug, Deserialize)]
struct ClaudeLimitScope {
    #[serde(default)]
    model: Option<ClaudeScopedModel>,
}

#[derive(Debug, Deserialize)]
struct ClaudeScopedModel {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeApiWindow {
    utilization: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeExtraUsage {
    #[serde(default)]
    is_enabled: bool,
    #[serde(default)]
    monthly_limit: Option<f64>,
    #[serde(default)]
    used_credits: Option<f64>,
    #[serde(default)]
    utilization: Option<f64>,
}

/// Parses Anthropic `/api/oauth/usage` JSON into normalized `UsageBucket`s (`wk`, `5h`,
/// `Sonnet`, `Opus`, `Cowork`, plus dynamic flattened buckets such as `Fable`) and `extra_usage`.
pub fn parse_claude_oauth_usage_json<T: TimeZone>(
    raw: &str,
    now: DateTime<Utc>,
    tz: &T,
) -> Result<(Vec<UsageBucket>, Option<CreditSummary>)>
where
    T::Offset: std::fmt::Display,
{
    let parsed: ClaudeUsageApiResponse =
        serde_json::from_str(raw).context("Failed to parse Claude OAuth usage JSON")?;

    let mut buckets: Vec<UsageBucket> = Vec::new();

    let push_window = |buckets: &mut Vec<UsageBucket>,
                       id: &str,
                       label: &str,
                       short_label: &str,
                       is_primary: bool,
                       win: &ClaudeApiWindow| {
        if let Some(util) = win.utilization {
            let norm_reset = win
                .resets_at
                .as_deref()
                .and_then(|r| reset::normalize_reset(r, now, tz));
            buckets.push(UsageBucket::new(
                id,
                label,
                short_label,
                BucketLayout::InlinePair,
                is_primary,
                util,
                norm_reset,
            ));
        }
    };

    if let Some(ref wk) = parsed.seven_day {
        push_window(&mut buckets, "wk", "Weekly", "wk", true, wk);
    }
    if let Some(ref fh) = parsed.five_hour {
        let is_primary = buckets.is_empty();
        push_window(&mut buckets, "5h", "Session (5h)", "5h", is_primary, fh);
    }
    if let Some(ref sonnet) = parsed.seven_day_sonnet {
        push_window(&mut buckets, "sonnet", "Sonnet", "Sonnet", false, sonnet);
    }
    if let Some(ref opus) = parsed.seven_day_opus {
        push_window(&mut buckets, "opus", "Opus", "Opus", false, opus);
    }
    if let Some(ref cowork) = parsed.seven_day_cowork {
        push_window(&mut buckets, "cowork", "Cowork", "Cowork", false, cowork);
    }

    // Dynamic flattened model buckets (e.g. `fable` or `seven_day_fable`), ignoring metadata & null staging flags.
    const IGNORED_META_KEYS: &[&str] = &[
        "spend",
        "seven_day_breakdown",
        "seven_day_oauth_apps",
        "limits",
        "weekly_scoped_shares",
        "pace",
        "indicator",
        "promo_credits",
        "member_dashboard_available",
    ];
    for (raw_key, val) in &parsed.extra_buckets {
        if IGNORED_META_KEYS.contains(&raw_key.as_str()) {
            continue;
        }
        let Some(obj) = val.as_object() else {
            continue;
        };
        let Some(util) = obj.get("utilization").and_then(|v| v.as_f64()) else {
            continue;
        };
        let resets_at_opt = obj.get("resets_at").and_then(|v| v.as_str());
        let norm_reset = resets_at_opt.and_then(|r| reset::normalize_reset(r, now, tz));

        let clean_key = raw_key
            .strip_prefix("seven_day_")
            .unwrap_or(raw_key.as_str());
        let id = slugify_id(clean_key);
        if id.is_empty() || buckets.iter().any(|b| b.id == id) {
            continue;
        }
        let title = humanize_model_bucket_label(clean_key);
        let is_primary = buckets.is_empty();
        buckets.push(UsageBucket::new(
            id,
            title.clone(),
            title,
            BucketLayout::InlinePair,
            is_primary,
            util,
            norm_reset,
        ));
    }

    // Claude Code 2.1.322+ `limits` array (`weekly_scoped` model limits such as `Fable`)
    for lim in &parsed.limits {
        if lim.kind != "weekly_scoped" {
            continue;
        }
        let Some(pct) = lim.percent else {
            continue;
        };
        let Some(raw_model_name) = lim
            .scope
            .as_ref()
            .and_then(|s| s.model.as_ref())
            .and_then(|m| m.display_name.as_deref().or(m.id.as_deref()))
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let id = slugify_id(raw_model_name);
        if id.is_empty() || buckets.iter().any(|b| b.id == id) {
            continue;
        }
        let title = humanize_model_bucket_label(raw_model_name);
        let norm_reset = lim
            .resets_at
            .as_deref()
            .and_then(|r| reset::normalize_reset(r, now, tz));
        let is_primary = buckets.is_empty();
        buckets.push(UsageBucket::new(
            id,
            title.clone(),
            title,
            BucketLayout::InlinePair,
            is_primary,
            pct,
            norm_reset,
        ));
    }

    let credits = parsed.extra_usage.and_then(|extra| {
        if !extra.is_enabled {
            return None;
        }
        let used_dollars = extra.used_credits.map(|c| c / 100.0);
        let limit_dollars = extra.monthly_limit.map(|l| l / 100.0);
        let used_percent = extra
            .utilization
            .or_else(|| {
                used_dollars
                    .zip(limit_dollars)
                    .filter(|(_, lim)| *lim > 0.0)
                    .map(|(u, l)| (u / l) * 100.0)
            })
            .map(|u| u.clamp(0.0, 100.0).round() as u8);

        Some(CreditSummary {
            label: "Extra usage".to_string(),
            used_dollars,
            limit_dollars,
            balance_dollars: None,
            used_percent,
        })
    });

    if buckets.is_empty() {
        return Err(anyhow!(
            "No active usage windows found in Claude OAuth response"
        ));
    }

    Ok((buckets, credits))
}

fn humanize_model_bucket_label(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_end_matches(" only")
        .trim_end_matches("_only")
        .trim();
    trimmed
        .split(['_', '-', ' '])
        .filter(|s| !s.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    let mut s = first.to_uppercase().collect::<String>();
                    s.push_str(&chars.as_str().to_ascii_lowercase());
                    s
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Tier 2: Multi-Bucket `claude -p /usage` CLI Fallback
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ParsedClaudeCliOutput {
    pub buckets: Vec<UsageBucket>,
    pub credits: Option<CreditSummary>,
    pub warning: Option<String>,
}

/// Parses multi-bucket text output from `claude -p /usage` (`Current week (all models)`,
/// `Current session`, `Current week (<Model>)`, `Extra usage`, and SSO expiry `Warning:` banners).
pub fn parse_claude_cli_multi_bucket<T: TimeZone>(
    raw: &str,
    now: DateTime<Utc>,
    tz: &T,
) -> Result<ParsedClaudeCliOutput>
where
    T::Offset: std::fmt::Display,
{
    let cleaned = strip_ansi(raw);

    let warning = cleaned
        .lines()
        .map(str::trim)
        .find(|line| line.to_ascii_lowercase().starts_with("warning:"))
        .map(String::from);

    let session_re = Regex::new(
        r"(?im)^Current\s+session\s*:\s*(\d+(?:\.\d+)?)\s*%\s*used(?:\s*[·•\-]\s*(.+))?$",
    )
    .expect("valid session regex");

    let week_re = Regex::new(
        r"(?im)^Current\s+week\s*\(\s*([^)]+?)\s*\)\s*:\s*(\d+(?:\.\d+)?)\s*%\s*used(?:\s*[·•\-]\s*(.+))?$",
    )
    .expect("valid week regex");

    let extra_re = Regex::new(
        r"(?im)^Extra\s+usage\s*:\s*\$(\d+(?:\.\d+)?)\s*/\s*\$(\d+(?:\.\d+)?)",
    )
    .expect("valid extra usage regex");

    let mut wk_bucket: Option<UsageBucket> = None;
    let mut session_bucket: Option<UsageBucket> = None;
    let mut model_buckets: Vec<UsageBucket> = Vec::new();

    for caps in week_re.captures_iter(&cleaned) {
        let scope_name = caps.get(1).map(|m| m.as_str().trim()).unwrap_or_default();
        let pct_val: f64 = caps
            .get(2)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0.0);
        let reset_norm = caps
            .get(3)
            .map(|m| m.as_str().trim())
            .and_then(|r| reset::normalize_reset(r, now, tz));

        if scope_name.eq_ignore_ascii_case("all models") {
            wk_bucket = Some(UsageBucket::new(
                "wk",
                "Weekly",
                "wk",
                BucketLayout::InlinePair,
                true,
                pct_val,
                reset_norm,
            ));
        } else {
            let label = humanize_model_bucket_label(scope_name);
            let id = slugify_id(&label);
            if !id.is_empty() && !model_buckets.iter().any(|b| b.id == id) {
                model_buckets.push(UsageBucket::new(
                    id,
                    label.clone(),
                    label,
                    BucketLayout::InlinePair,
                    false,
                    pct_val,
                    reset_norm,
                ));
            }
        }
    }

    if let Some(caps) = session_re.captures(&cleaned) {
        let pct_val: f64 = caps
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0.0);
        let reset_norm = caps
            .get(2)
            .map(|m| m.as_str().trim())
            .and_then(|r| reset::normalize_reset(r, now, tz));
        session_bucket = Some(UsageBucket::new(
            "5h",
            "Session (5h)",
            "5h",
            BucketLayout::InlinePair,
            wk_bucket.is_none(),
            pct_val,
            reset_norm,
        ));
    }

    let mut buckets = Vec::new();
    if let Some(wk) = wk_bucket {
        buckets.push(wk);
    }
    if let Some(fh) = session_bucket {
        buckets.push(fh);
    }
    buckets.extend(model_buckets);

    if !buckets.iter().any(|b| b.is_primary) {
        if let Some(first) = buckets.first_mut() {
            first.is_primary = true;
        }
    }

    let credits = extra_re.captures(&cleaned).and_then(|caps| {
        let used: f64 = caps.get(1)?.as_str().parse().ok()?;
        let limit: f64 = caps.get(2)?.as_str().parse().ok()?;
        let pct = if limit > 0.0 {
            Some(((used / limit) * 100.0).clamp(0.0, 100.0).round() as u8)
        } else {
            None
        };
        Some(CreditSummary {
            label: "Extra usage".to_string(),
            used_dollars: Some(used),
            limit_dollars: Some(limit),
            balance_dollars: None,
            used_percent: pct,
        })
    });

    if buckets.is_empty() {
        let snippet: String = cleaned
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(2)
            .collect::<Vec<_>>()
            .join(" | ");
        if snippet.is_empty() {
            return Err(anyhow!("Empty output from claude -p /usage"));
        }
        return Err(anyhow!("Could not parse claude usage output: {}", snippet));
    }

    Ok(ParsedClaudeCliOutput {
        buckets,
        credits,
        warning,
    })
}

fn fetch_via_claude_cli(
    account: &AccountDescriptor,
    profile_path: Option<&Path>,
    resolved_label: &str,
    plan_tier: Option<String>,
    ctx: &FetchContext,
) -> AccountFetchOutcome {
    let bin = resolve_binary_for("claude", account.binary.as_deref());
    let mut spec = CommandSpec::new(bin, ctx.timeout).args(["-p", "/usage"]);

    if let Some(p) = profile_path {
        let dir = if p.is_file() {
            p.parent().unwrap_or(p)
        } else {
            p
        };
        spec = spec.env("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned());
    }

    let output = match run_command(&spec) {
        Ok(o) => o,
        Err(err) => {
            return AccountFetchOutcome::Error {
                message: format!("{:#}", err),
            };
        }
    };

    let combined = output.combined_text();
    match parse_claude_cli_multi_bucket(&combined, ctx.now, &Local) {
        Ok(parsed) => {
            let state = if parsed.warning.is_some() {
                AccountState::ExpiringSoon
            } else {
                AccountState::ActiveOk
            };
            AccountFetchOutcome::Success(AccountReport {
                id: account.id.clone(),
                label: resolved_label.to_string(),
                source_label: account.source_label.clone(),
                is_system_default: account.is_system_default,
                is_active: false,
                plan_tier,
                state,
                is_stale: false,
                status_note: parsed.warning,
                error: None,
                login_command: account.login_command.clone(),
                updated_at_epoch: Some(ctx.now_epoch()),
                buckets: parsed.buckets,
                credits: parsed.credits,
            })
        }
        Err(err) => {
            let msg = format!("{:#}", err);
            let lower = combined.to_ascii_lowercase();
            if lower.contains("auth login")
                || lower.contains("not logged in")
                || lower.contains("please run /login")
                || lower.contains("authentication_error")
                || lower.contains("invalid api key")
            {
                AccountFetchOutcome::SignedOut {
                    message: msg,
                    login_command: account.login_command.clone(),
                    label: Some(resolved_label.to_string()),
                    plan_tier,
                }
            } else {
                AccountFetchOutcome::Error { message: msg }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tier 1A: Passive `statusLine` Hook Ingestion (`ingest-hook claude`)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct StatusLinePayload {
    #[serde(default)]
    rate_limits: Option<StatusLineRateLimits>,
}

#[derive(Debug, Deserialize)]
struct StatusLineRateLimits {
    #[serde(default)]
    five_hour: Option<StatusLineWindow>,
    #[serde(default)]
    seven_day: Option<StatusLineWindow>,
}

#[derive(Debug, Deserialize)]
struct StatusLineWindow {
    #[serde(alias = "used_percent", alias = "utilization")]
    used_percentage: Option<f64>,
    #[serde(default)]
    resets_at: Option<serde_json::Value>,
}

fn parse_statusline_reset<T: TimeZone>(
    val: Option<&serde_json::Value>,
    now: DateTime<Utc>,
    tz: &T,
) -> Option<(i64, String)>
where
    T::Offset: std::fmt::Display,
{
    let v = val?;
    if let Some(ts) = v.as_i64() {
        return reset::normalize_epoch(ts, tz);
    }
    if let Some(f) = v.as_f64() {
        return reset::normalize_epoch(f.round() as i64, tz);
    }
    if let Some(s) = v.as_str() {
        return reset::normalize_reset(s, now, tz);
    }
    None
}

/// Parses Claude Code `statusLine` JSON from `raw_stdin` and atomically updates passive
/// `wk` and `5h` buckets in `collector-cache.json` and `state.json` (`0600`) with zero network I/O.
pub fn ingest_claude_statusline(
    cfg: &Config,
    cache_store: &CacheStore,
    account_hint: Option<&str>,
    raw_stdin: &str,
    now: DateTime<Utc>,
) -> Result<StatusReport> {
    let payload: StatusLinePayload =
        serde_json::from_str(raw_stdin.trim()).context("Invalid Claude statusLine JSON on stdin")?;
    let Some(limits) = payload.rate_limits else {
        // Not all statusLine events carry rate_limits; return current cached state cleanly.
        return Ok(cache_store.load_cached_report(&cfg.separator, now.timestamp()));
    };

    let mut new_wk: Option<UsageBucket> = None;
    let mut new_5h: Option<UsageBucket> = None;

    if let Some(ref sd) = limits.seven_day {
        if let Some(pct) = sd.used_percentage {
            let norm = parse_statusline_reset(sd.resets_at.as_ref(), now, &Local);
            new_wk = Some(UsageBucket::new(
                "wk",
                "Weekly",
                "wk",
                BucketLayout::InlinePair,
                true,
                pct,
                norm,
            ));
        }
    }
    if let Some(ref fh) = limits.five_hour {
        if let Some(pct) = fh.used_percentage {
            let norm = parse_statusline_reset(fh.resets_at.as_ref(), now, &Local);
            new_5h = Some(UsageBucket::new(
                "5h",
                "Session (5h)",
                "5h",
                BucketLayout::InlinePair,
                new_wk.is_none(),
                pct,
                norm,
            ));
        }
    }

    if new_wk.is_none() && new_5h.is_none() {
        return Ok(cache_store.load_cached_report(&cfg.separator, now.timestamp()));
    }

    let now_epoch = now.timestamp();
    let _lock = cache_store.lock()?;
    let mut collector_state = cache_store.load_collector_cache();

    // Resolve target account key in collector_state
    let target_key = resolve_hook_target_account_key(&collector_state, account_hint, cfg);
    let existing = collector_state
        .accounts
        .entry(target_key.clone())
        .or_insert_with(|| {
            let clean_label = target_key
                .strip_prefix("claude:")
                .unwrap_or(&target_key)
                .to_string();
            CachedAccountState {
                provider_id: "claude".to_string(),
                account_id: target_key.clone(),
                label: if clean_label == "default" {
                    "Claude".to_string()
                } else {
                    clean_label
                },
                source_label: Some("~/.claude".to_string()),
                is_system_default: account_hint.is_none(),
                plan_tier: None,
                state: AccountState::ActiveOk,
                status_note: None,
                error: None,
                login_command: Some("claude auth login".to_string()),
                last_fetch_attempt_epoch: Some(now_epoch),
                last_success_epoch: Some(now_epoch),
                retry_after_epoch: None,
                consecutive_429s: 0,
                buckets: Vec::new(),
                credits: None,
            }
        });

    // Preserve any existing model buckets (`fable`, `sonnet`, `opus`) while updating `wk` and `5h`.
    let preserved_models: Vec<UsageBucket> = existing
        .buckets
        .iter()
        .filter(|b| b.id != "wk" && b.id != "5h")
        .cloned()
        .map(|mut b| {
            b.is_primary = false;
            b
        })
        .collect();

    let preserved_wk = existing.buckets.iter().find(|b| b.id == "wk").cloned();
    let preserved_5h = existing.buckets.iter().find(|b| b.id == "5h").cloned();

    let mut merged_buckets = Vec::new();
    if let Some(wk) = new_wk.or(preserved_wk) {
        let mut wk = wk;
        wk.is_primary = true;
        merged_buckets.push(wk);
    }
    if let Some(mut fh) = new_5h.or(preserved_5h) {
        fh.is_primary = merged_buckets.is_empty();
        merged_buckets.push(fh);
    }
    merged_buckets.extend(preserved_models);

    existing.buckets = merged_buckets;
    existing.state = AccountState::ActiveOk;
    existing.error = None;
    existing.status_note = None;
    existing.last_success_epoch = Some(now_epoch);
    existing.last_fetch_attempt_epoch = Some(now_epoch);

    cache_store.save_collector_cache(&collector_state)?;

    // Update existing `state.json` if present (preserving other providers), else synthesize from cache!
    let mut report = if let Ok(raw) = fs::read_to_string(cache_store.state_path()) {
        if let Ok(mut prev) = serde_json::from_str::<StatusReport>(&raw) {
            let updated_acct = collector_state.accounts[&target_key].to_account_report(false, now_epoch);
            if let Some(prov) = prev.providers.iter_mut().find(|p| p.id == "claude") {
                if let Some(slot) = prov.accounts.iter_mut().find(|a| a.id == target_key) {
                    *slot = updated_acct;
                } else {
                    prov.accounts.push(updated_acct);
                }
            } else {
                let pin = collector_state.active_accounts.get("claude").map(String::as_str);
                prev.providers.insert(
                    0,
                    crate::model::ProviderReport::from_accounts(
                        "claude",
                        "Claude",
                        "provider-claude",
                        Some("https://claude.ai/settings/usage".to_string()),
                        vec![updated_acct],
                        pin,
                    ),
                );
            }
            prev.updated_at_epoch = now_epoch;
            apply_report_time_decay(
                &mut prev,
                &cfg.separator,
                now_epoch,
                Some(&collector_state.active_accounts),
            );
            prev
        } else {
            synthesize_report_from_cache(&collector_state, &cfg.separator, now_epoch)
        }
    } else {
        synthesize_report_from_cache(&collector_state, &cfg.separator, now_epoch)
    };

    report.updated_at_epoch = now_epoch;
    cache_store.save_state_report(&report)?;
    Ok(report)
}

fn resolve_hook_target_account_key(
    cache: & crate::cache::CollectorState,
    account_hint: Option<&str>,
    cfg: &Config,
) -> String {
    if let Some(hint) = account_hint.map(str::trim).filter(|s| !s.is_empty()) {
        let prefixed = if hint.starts_with("claude:") {
            hint.to_string()
        } else {
            format!("claude:{hint}")
        };
        if cache.accounts.contains_key(&prefixed) {
            return prefixed;
        }
        if let Some(found) = cache
            .accounts
            .values()
            .find(|a| a.provider_id == "claude" && a.label.eq_ignore_ascii_case(hint))
        {
            return found.account_id.clone();
        }
        return prefixed;
    }

    if let Some(pinned) = cache.active_accounts.get("claude") {
        if cache.accounts.contains_key(pinned) {
            return pinned.clone();
        }
    }

    if let Some(sys_def) = cache
        .accounts
        .values()
        .find(|a| a.provider_id == "claude" && a.is_system_default)
    {
        return sys_def.account_id.clone();
    }

    if let Some(any_claude) = cache.accounts.values().find(|a| a.provider_id == "claude") {
        return any_claude.account_id.clone();
    }

    // Check if ~/.claude has an email identity on disk
    let home = crate::config::home_dir();
    if let Some(ref h) = home {
        if let Some(id) = read_claude_identity(&h.join(".claude"), Some(h)) {
            if let Some(email) = id.email {
                return format!("claude:{}", email.to_ascii_lowercase());
            }
        }
    }

    let (_, manual) = cfg.effective_provider_accounts("claude");
    if let Some(first_manual) = manual.first() {
        return format!("claude:{}", first_manual.id);
    }

    "claude:default".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    const SAMPLE_CLAUDE_OAUTH_JSON: &str = r#"{
      "five_hour": {
        "utilization": 37.0,
        "resets_at": "2026-11-11T04:59:59.000000+00:00"
      },
      "seven_day": {
        "utilization": 89.0,
        "resets_at": "2026-11-15T14:59:59.771647+00:00"
      },
      "seven_day_sonnet": {
        "utilization": 12.0,
        "resets_at": "2026-11-16T20:59:59.771655+00:00"
      },
      "seven_day_opus": null,
      "seven_day_oauth_apps": null,
      "iguana_necktie": null,
      "fable": {
        "utilization": 46.0,
        "resets_at": "2026-11-15T14:59:59.771647+00:00"
      },
      "extra_usage": {
        "is_enabled": true,
        "monthly_limit": 10000,
        "used_credits": 1450,
        "utilization": 14.5
      }
    }"#;

    #[test]
    fn test_parse_claude_oauth_usage_with_dynamic_fable_and_extra_usage() {
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 18, 0, 0).unwrap();
        let pst = FixedOffset::west_opt(8 * 3600).unwrap();

        let (buckets, credits) =
            parse_claude_oauth_usage_json(SAMPLE_CLAUDE_OAUTH_JSON, now, &pst)
                .expect("parse claude oauth json");

        assert_eq!(buckets.len(), 4);
        assert_eq!(buckets[0].id, "wk");
        assert!(buckets[0].is_primary);
        assert_eq!(buckets[0].used_percent, 89);

        assert_eq!(buckets[1].id, "5h");
        assert!(!buckets[1].is_primary);
        assert_eq!(buckets[1].used_percent, 37);

        assert_eq!(buckets[2].id, "sonnet");
        assert_eq!(buckets[2].used_percent, 12);

        assert_eq!(buckets[3].id, "fable");
        assert_eq!(buckets[3].label, "Fable");
        assert_eq!(buckets[3].used_percent, 46);

        let cred = credits.expect("extra_usage enabled");
        assert_eq!(cred.label, "Extra usage");
        assert_eq!(cred.used_dollars, Some(14.5));
        assert_eq!(cred.limit_dollars, Some(100.0));
        assert_eq!(cred.used_percent, Some(15));
    }

    #[test]
    fn test_parse_claude_oauth_v2_1_322_scoped_limits_fable_and_metadata_ignore() {
        let raw = r#"{
          "five_hour": null,
          "seven_day": {
            "utilization": 3.0,
            "resets_at": "2026-11-16T05:00:00+00:00"
          },
          "spend": { "utilization": 99.0 },
          "pace": { "utilization": 50.0 },
          "limits": [
            {
              "kind": "weekly_total",
              "group": "weekly",
              "percent": 3,
              "resets_at": "2026-11-16T05:00:00+00:00",
              "scope": null,
              "is_active": true
            },
            {
              "kind": "weekly_scoped",
              "group": "weekly",
              "percent": 0,
              "resets_at": "2026-11-16T05:00:00+00:00",
              "scope": {
                "model": { "id": "fable", "display_name": "Fable" }
              },
              "is_active": false
            }
          ],
          "extra_usage": {
            "is_enabled": false,
            "monthly_limit": null,
            "used_credits": null,
            "utilization": null
          }
        }"#;
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 18, 0, 0).unwrap();
        let (buckets, credits) =
            parse_claude_oauth_usage_json(raw, now, &Utc).expect("parse v2.1.322 oauth usage");
        assert!(credits.is_none());
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].id, "wk");
        assert_eq!(buckets[0].used_percent, 3);
        assert_eq!(buckets[1].id, "fable");
        assert_eq!(buckets[1].label, "Fable");
        assert_eq!(buckets[1].used_percent, 0);
    }

    #[test]
    fn test_parse_claude_cli_multi_bucket_with_sso_warning() {
        let raw = r#"Warning: this sign-in ends between 2026-11-07 and 2026-11-10 (in 2 days at the earliest).
Current session: 24% used · resets 9pm (UTC)
Current week (all models): 89% used · resets Nov 10, 7am (UTC)
Current week (Fable): 46% used · resets Nov 10, 7am (UTC)
Current week (Sonnet only): 15% used · resets Nov 10, 7am (UTC)
Extra usage: $14.50 / $100.00
"#;
        let now = Utc.with_ymd_and_hms(2026, 11, 6, 16, 0, 0).unwrap();
        let parsed =
            parse_claude_cli_multi_bucket(raw, now, &Utc).expect("parse multi-bucket claude cli");

        assert!(parsed
            .warning
            .as_deref()
            .unwrap_or_default()
            .starts_with("Warning: this sign-in ends"));
        assert_eq!(parsed.buckets.len(), 4);
        assert_eq!(parsed.buckets[0].id, "wk");
        assert_eq!(parsed.buckets[0].used_percent, 89);
        assert_eq!(parsed.buckets[1].id, "5h");
        assert_eq!(parsed.buckets[1].used_percent, 24);
        assert_eq!(parsed.buckets[2].id, "fable");
        assert_eq!(parsed.buckets[2].used_percent, 46);
        assert_eq!(parsed.buckets[3].id, "sonnet");
        assert_eq!(parsed.buckets[3].label, "Sonnet");
        assert_eq!(parsed.buckets[3].used_percent, 15);

        let cred = parsed.credits.expect("extra usage");
        assert_eq!(cred.used_dollars, Some(14.5));
        assert_eq!(cred.limit_dollars, Some(100.0));
    }

    #[test]
    fn test_ingest_claude_statusline_updates_passive_cache_and_preserves_fable() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "gas-claude-hook-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = CacheStore::new(tmp_dir.clone());
        let cfg = Config::default();
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 18, 0, 0).unwrap();

        // Seed cache with an existing `fable` model bucket on `claude:work`
        let mut init_cache = crate::cache::CollectorState::default();
        init_cache.accounts.insert(
            "claude:work".to_string(),
            CachedAccountState {
                provider_id: "claude".to_string(),
                account_id: "claude:work".to_string(),
                label: "work@example.com".to_string(),
                source_label: Some("~/.claude-work".to_string()),
                is_system_default: false,
                plan_tier: Some("Max 20x".to_string()),
                state: AccountState::PassiveCached,
                status_note: Some("Updates when Claude runs".to_string()),
                error: None,
                login_command: Some("CLAUDE_CONFIG_DIR=~/.claude-work claude auth login".to_string()),
                last_fetch_attempt_epoch: Some(1000),
                last_success_epoch: Some(1000),
                retry_after_epoch: None,
                consecutive_429s: 0,
                buckets: vec![UsageBucket::new(
                    "fable",
                    "Fable",
                    "Fable",
                    BucketLayout::InlinePair,
                    false,
                    46.0,
                    Some((1794807600, "2026-11-15T14:59:59Z".to_string())),
                )],
                credits: None,
            },
        );
        store.save_collector_cache(&init_cache).unwrap();

        let hook_stdin = r#"{
            "rate_limits": {
                "five_hour": { "used_percentage": 23.6, "resets_at": 1794375600 },
                "seven_day": { "used_percentage": 81.2, "resets_at": 1794807600 }
            }
        }"#;

        let report = ingest_claude_statusline(&cfg, &store, Some("work"), hook_stdin, now)
            .expect("ingest statusline");

        assert_eq!(report.providers.len(), 1);
        assert_eq!(report.providers[0].id, "claude");
        assert_eq!(report.providers[0].primary_used_percent, Some(81));
        assert_eq!(report.status, crate::model::OverallStatus::Warning);

        let acct = &report.providers[0].accounts[0];
        assert_eq!(acct.buckets.len(), 3);
        assert_eq!(acct.buckets[0].id, "wk");
        assert_eq!(acct.buckets[0].used_percent, 81);
        assert_eq!(acct.buckets[1].id, "5h");
        assert_eq!(acct.buckets[1].used_percent, 24);
        assert_eq!(acct.buckets[2].id, "fable");
        assert_eq!(acct.buckets[2].used_percent, 46);

        let _ = fs::remove_dir_all(&tmp_dir);
    }
}
