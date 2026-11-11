//! OpenAI Codex (`codex`) usage provider:
//! - Tier 1A: Direct ChatGPT WHAM Usage API (`GET https://chatgpt.com/backend-api/wham/usage` + `ChatGPT-Account-Id`)
//! - Tier 1B: Official `codex app-server` JSON-RPC (`account/rateLimits/read`, keyring & safe token rotation)
//! - Tier 2: Offline session rollout JSONL / `.jsonl.zst` scanner (`$CODEX_HOME/sessions/**/rollout-*.jsonl[.zst]`)

use crate::cache::CachedAccountState;
use crate::config::{
    abbreviate_home, discover_provider_locations, expand_tilde_with_home, resolve_binary_for,
    ManualAccountConfig,
};
use crate::http::decode_jwt_payload;
use crate::model::{
    AccountReport, AccountState, BucketLayout, CreditSummary, UsageBucket,
};
use crate::proc_exec::{run_jsonrpc_line_query, CommandSpec};
use crate::providers::{
    AccountDescriptor, AccountFetchOutcome, FetchContext, ProviderMeta, UsageProvider,
};
use crate::reset;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local, TimeZone, Utc};
use serde::Deserialize;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

const DEFAULT_CODEX_WHAM_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

pub struct CodexProvider;

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexProvider {
    pub fn new() -> Self {
        Self
    }
}

impl UsageProvider for CodexProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            id: "codex",
            display_name: "Codex",
            icon_name: "provider-codex",
            dashboard_url: "https://chatgpt.com/codex/settings/usage",
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
        let default_codex_dir = home.map(|h| h.join(".codex"));

        for m in manual {
            let expanded = m
                .path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|p| expand_tilde_with_home(p, home));

            let is_sys_default = match (&expanded, &default_codex_dir) {
                (Some(p), Some(def)) => p == def,
                (None, _) => true,
                _ => false,
            };

            let auth_info = expanded.as_deref().and_then(read_codex_auth_info);
            let source_label = expanded.as_deref().map(|p| abbreviate_home(p, home));
            let login_cmd = build_codex_login_command(
                expanded.as_deref(),
                source_label.as_deref(),
                is_sys_default,
            );

            let label = if m.label.as_deref().is_some_and(|l| !l.trim().is_empty()) {
                m.display_label()
            } else {
                auth_info
                    .as_ref()
                    .and_then(|a| a.email.clone())
                    .unwrap_or_else(|| m.display_label())
            };

            let dedup_key = auth_info.and_then(|a| a.dedup_identity());

            out.push(AccountDescriptor {
                provider_id: "codex".to_string(),
                id: format!("codex:{}", m.id),
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
            for loc in discover_provider_locations("codex", home, ctx.xdg_config_dir.as_deref()) {
                let auth_info = read_codex_auth_info(&loc.path);
                // If this is the OpenCode shared `auth.json` file, only include it when an `openai` entry exists!
                if loc.suggested_id == "opencode" && auth_info.is_none() {
                    continue;
                }

                let email = auth_info.as_ref().and_then(|a| a.email.clone());
                let acct_id = if let Some(ref e) = email {
                    format!("codex:{}", e.to_ascii_lowercase())
                } else {
                    format!("codex:{}", loc.suggested_id)
                };
                let label = email.unwrap_or_else(|| {
                    if loc.is_system_default {
                        "Codex".to_string()
                    } else {
                        loc.suggested_id.clone()
                    }
                });
                let dedup_key = auth_info
                    .and_then(|a| a.dedup_identity())
                    .or_else(|| Some(loc.path.to_string_lossy().into_owned()));
                let login_cmd = build_codex_login_command(
                    Some(&loc.path),
                    Some(&loc.source_label),
                    loc.is_system_default,
                );

                out.push(AccountDescriptor {
                    provider_id: "codex".to_string(),
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
            .or_else(|| home.map(|h| h.join(".codex")));

        let mut auth_info = profile_path.as_deref().and_then(read_codex_auth_info);

        // If JWT is expired, re-read once from disk in case an active `codex` CLI already refreshed it.
        if auth_info
            .as_ref()
            .is_some_and(|a| a.is_expired(ctx.now.timestamp()))
        {
            auth_info = profile_path.as_deref().and_then(read_codex_auth_info);
        }

        let jwt_plan = auth_info.as_ref().and_then(|a| a.plan_tier.clone());
        let jwt_email = auth_info.as_ref().and_then(|a| a.email.clone());
        let base_label = if account.label == "Codex" || account.label == "default" {
            jwt_email.clone().unwrap_or_else(|| account.label.clone())
        } else {
            account.label.clone()
        };

        // Tier 1A: Direct WHAM Usage HTTP call when a valid non-expired `access_token` exists
        if let Some(ref auth) = auth_info {
            if let Some(ref access_token) = auth.access_token {
                if !access_token.is_empty() && !auth.is_expired(ctx.now.timestamp()) {
                    let wham_url = ctx
                        .endpoints
                        .codex_wham_url
                        .as_deref()
                        .unwrap_or(DEFAULT_CODEX_WHAM_URL);
                    let bearer = format!("Bearer {access_token}");
                    let mut headers = vec![
                        ("Authorization", bearer.as_str()),
                        ("Accept", "application/json"),
                        ("User-Agent", "codex-cli/0.2.0"),
                    ];
                    if let Some(ref acct_hdr) = auth.account_id {
                        if !acct_hdr.is_empty() {
                            headers.push(("ChatGPT-Account-Id", acct_hdr.as_str()));
                        }
                    }

                    match ctx.http.get(wham_url, &headers) {
                        Ok(resp) if resp.is_success() => {
                            match parse_codex_wham_usage(&resp.body, &Local) {
                                Ok(parsed) => {
                                    let final_label = if account.label == "Codex"
                                        || account.label == "default"
                                    {
                                        parsed
                                            .email
                                            .or(jwt_email)
                                            .unwrap_or_else(|| base_label.clone())
                                    } else {
                                        base_label
                                    };
                                    return AccountFetchOutcome::Success(AccountReport {
                                        id: account.id.clone(),
                                        label: final_label,
                                        source_label: account.source_label.clone(),
                                        is_system_default: account.is_system_default,
                                        is_active: false,
                                        plan_tier: parsed.plan_tier.or(jwt_plan),
                                        state: AccountState::ActiveOk,
                                        is_stale: false,
                                        status_note: None,
                                        extra_info: None,
                                        error: None,
                                        login_command: account.login_command.clone(),
                                        updated_at_epoch: Some(ctx.now_epoch()),
                                        buckets: parsed.buckets,
                                        credits: parsed.credits,
                                    });
                                }
                                Err(err) => {
                                    return AccountFetchOutcome::Error {
                                        message: format!("{:#}", err),
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
                            // Fall through to `codex app-server` so Codex can rotate token if possible!
                        }
                        Ok(_) | Err(_) => {
                            // Fall through to `codex app-server` / rollout JSONL fallback
                        }
                    }
                }
            }
        }

        // Tier 1B: `codex app-server` JSON-RPC (`account/rateLimits/read`)
        let codex_bin = resolve_binary_for("codex", account.binary.as_deref());
        if codex_bin.is_file() || account.binary.is_some() {
            if let Ok(parsed) =
                fetch_via_codex_app_server(&codex_bin, profile_path.as_deref(), ctx)
            {
                return AccountFetchOutcome::Success(AccountReport {
                    id: account.id.clone(),
                    label: base_label,
                    source_label: account.source_label.clone(),
                    is_system_default: account.is_system_default,
                    is_active: false,
                    plan_tier: parsed.plan_tier.or(jwt_plan),
                    state: AccountState::ActiveOk,
                    is_stale: false,
                    status_note: None,
                    extra_info: None,
                    error: None,
                    login_command: account.login_command.clone(),
                    updated_at_epoch: Some(ctx.now_epoch()),
                    buckets: parsed.buckets,
                    credits: parsed.credits,
                });
            }
        }

        // Tier 2: Offline session rollout JSONL backward scanner (`$CODEX_HOME/sessions/**/*.jsonl`)
        if let Some(ref p) = profile_path {
            if let Some(buckets) = scan_codex_rollout_sessions(p, ctx.now, &Local) {
                return AccountFetchOutcome::Success(AccountReport {
                    id: account.id.clone(),
                    label: base_label,
                    source_label: account.source_label.clone(),
                    is_system_default: account.is_system_default,
                    is_active: false,
                    plan_tier: jwt_plan,
                    state: AccountState::ActiveOk,
                    is_stale: true,
                    status_note: Some("From local Codex session log".to_string()),
                    extra_info: None,
                    error: None,
                    login_command: account.login_command.clone(),
                    updated_at_epoch: Some(ctx.now_epoch()),
                    buckets,
                    credits: None,
                });
            }
        }

        // Check if API-key only mode vs SignedOut
        if auth_info.as_ref().is_some_and(|a| a.is_api_key_only) {
            return AccountFetchOutcome::Error {
                message: "OpenAI API-key mode does not expose subscription window quotas".to_string(),
            };
        }

        AccountFetchOutcome::SignedOut {
            message: "Codex credentials missing or expired".to_string(),
            login_command: account.login_command.clone(),
            label: Some(base_label),
            plan_tier: jwt_plan,
        }
    }
}

// ---------------------------------------------------------------------------
// Auth & JWT Claim Extraction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CodexAuthInfo {
    access_token: Option<String>,
    account_id: Option<String>,
    email: Option<String>,
    plan_tier: Option<String>,
    exp_epoch: Option<i64>,
    is_api_key_only: bool,
}

impl CodexAuthInfo {
    fn is_expired(&self, now_epoch: i64) -> bool {
        self.exp_epoch
            .is_some_and(|exp| now_epoch >= exp.saturating_sub(60))
    }

    fn dedup_identity(&self) -> Option<String> {
        match (&self.account_id, &self.email) {
            (Some(acct), Some(email)) => Some(format!("{acct}:{}", email.to_ascii_lowercase())),
            (None, Some(email)) => Some(email.to_ascii_lowercase()),
            (Some(acct), None) => Some(acct.clone()),
            (None, None) => None,
        }
    }
}

fn read_codex_auth_info(path: &Path) -> Option<CodexAuthInfo> {
    let auth_file = if path.is_file() {
        path.to_path_buf()
    } else {
        path.join("auth.json")
    };
    let raw = fs::read_to_string(auth_file).ok()?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;

    // Shape A: OpenCode `~/.local/share/opencode/auth.json` -> `openai` key
    if let Some(openai_obj) = val.get("openai").and_then(|v| v.as_object()) {
        let access = openai_obj
            .get("access")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)?;
        let acct_id = openai_obj
            .get("accountId")
            .and_then(|v| v.as_str())
            .map(String::from);
        let expires_raw = openai_obj.get("expires").and_then(|v| v.as_i64());
        let exp_secs = expires_raw.map(|ts| if ts > 1_000_000_000_000 { ts / 1000 } else { ts });
        let (jwt_email, jwt_plan, jwt_acct, jwt_exp) = extract_openai_jwt_claims(&access);

        return Some(CodexAuthInfo {
            access_token: Some(access),
            account_id: acct_id.or(jwt_acct),
            email: jwt_email,
            plan_tier: jwt_plan,
            exp_epoch: exp_secs.or(jwt_exp),
            is_api_key_only: false,
        });
    }

    // Shape B: Official `$CODEX_HOME/auth.json`
    let auth_mode = val
        .get("auth_mode")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let has_api_key = val
        .get("OPENAI_API_KEY")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty());

    if let Some(tokens) = val.get("tokens").and_then(|v| v.as_object()) {
        let access_token = tokens
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let id_token = tokens
            .get("id_token")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let explicit_acct_id = tokens
            .get("account_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);

        let (id_email, id_plan, id_acct, _) = id_token
            .map(extract_openai_jwt_claims)
            .unwrap_or((None, None, None, None));
        let (acc_email, acc_plan, acc_acct, acc_exp) = access_token
            .as_deref()
            .map(extract_openai_jwt_claims)
            .unwrap_or((None, None, None, None));

        return Some(CodexAuthInfo {
            access_token,
            account_id: explicit_acct_id.or(id_acct).or(acc_acct),
            email: id_email.or(acc_email),
            plan_tier: id_plan.or(acc_plan),
            exp_epoch: acc_exp,
            is_api_key_only: false,
        });
    }

    if auth_mode.eq_ignore_ascii_case("apikey") || has_api_key {
        return Some(CodexAuthInfo {
            access_token: None,
            account_id: None,
            email: None,
            plan_tier: Some("API".to_string()),
            exp_epoch: None,
            is_api_key_only: true,
        });
    }

    None
}

fn extract_openai_jwt_claims(
    jwt: &str,
) -> (Option<String>, Option<String>, Option<String>, Option<i64>) {
    let Some(claims) = decode_jwt_payload(jwt) else {
        return (None, None, None, None);
    };
    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let auth_ns = claims.get("https://api.openai.com/auth");
    let raw_plan = auth_ns
        .and_then(|ns| ns.get("chatgpt_plan_type"))
        .and_then(|v| v.as_str());
    let plan_tier = raw_plan.and_then(normalize_codex_plan);
    let acct_id = auth_ns
        .and_then(|ns| ns.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let exp = claims.get("exp").and_then(|v| v.as_i64());

    (email, plan_tier, acct_id, exp)
}

fn normalize_codex_plan(raw: &str) -> Option<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "free" => None,
        "pro" => Some("Pro".to_string()),
        "plus" => Some("Plus".to_string()),
        "team" => Some("Team".to_string()),
        "enterprise" | "edu" => Some("Enterprise".to_string()),
        other => {
            let mut chars = other.chars();
            chars.next().map(|c| {
                let mut s = c.to_uppercase().collect::<String>();
                s.push_str(chars.as_str());
                s
            })
        }
    }
}

fn build_codex_login_command(
    path: Option<&Path>,
    source_label: Option<&str>,
    is_system_default: bool,
) -> String {
    if is_system_default {
        return "codex login".to_string();
    }
    let dir_str = source_label
        .map(String::from)
        .or_else(|| path.map(|p| p.to_string_lossy().into_owned()));
    match dir_str {
        Some(d) if !d.is_empty() && !d.ends_with("opencode/auth.json") => {
            if d.contains(' ') {
                format!("CODEX_HOME=\"{d}\" codex login")
            } else {
                format!("CODEX_HOME={d} codex login")
            }
        }
        _ => "codex login".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tier 1A: WHAM Usage Response Parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ParsedCodexUsage {
    pub email: Option<String>,
    pub plan_tier: Option<String>,
    pub buckets: Vec<UsageBucket>,
    pub credits: Option<CreditSummary>,
}

#[derive(Debug, Deserialize)]
struct WhamUsageResponse {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    plan_type: Option<String>,
    #[serde(default)]
    rate_limit: Option<WhamRateLimit>,
    #[serde(default)]
    code_review_rate_limit: Option<WhamRateLimit>,
    #[serde(default)]
    credits: Option<WhamCredits>,
}

#[derive(Debug, Deserialize)]
struct WhamRateLimit {
    #[serde(default)]
    primary_window: Option<WhamWindow>,
    #[serde(default)]
    secondary_window: Option<WhamWindow>,
}

#[derive(Debug, Deserialize)]
struct WhamWindow {
    used_percent: Option<f64>,
    #[serde(default)]
    limit_window_seconds: Option<i64>,
    #[serde(default)]
    reset_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct WhamCredits {
    #[serde(default)]
    has_credits: bool,
    #[serde(default)]
    balance: Option<serde_json::Value>,
}

pub fn parse_codex_wham_usage<T: TimeZone>(raw: &str, tz: &T) -> Result<ParsedCodexUsage>
where
    T::Offset: std::fmt::Display,
{
    let parsed: WhamUsageResponse =
        serde_json::from_str(raw).context("Failed to parse Codex wham/usage JSON")?;

    let mut buckets = Vec::new();
    if let Some(ref rl) = parsed.rate_limit {
        if let Some(ref sec) = rl.secondary_window {
            if let Some(pct) = sec.used_percent {
                let norm = sec.reset_at.and_then(|ts| reset::normalize_epoch(ts, tz));
                let win = sec
                    .limit_window_seconds
                    .and_then(|s| u64::try_from(s).ok())
                    .filter(|&s| s > 0)
                    .or(Some(604_800));
                buckets.push(
                    UsageBucket::new(
                        "wk",
                        "Weekly",
                        "wk",
                        BucketLayout::InlinePair,
                        true,
                        pct,
                        norm,
                    )
                    .with_window_seconds(win),
                );
            }
        }
        if let Some(ref pri) = rl.primary_window {
            if let Some(pct) = pri.used_percent {
                let norm = pri.reset_at.and_then(|ts| reset::normalize_epoch(ts, tz));
                let is_primary = buckets.is_empty();
                // ChatGPT Pro plans expose a single 7-day (`604800`s) window in `primary_window`
                // with `secondary_window: null`. Classify >= 5-day windows as `wk` ("Weekly").
                let is_weekly_single =
                    buckets.is_empty() && pri.limit_window_seconds.is_some_and(|s| s >= 432_000);
                let (id, label, short_label, default_win) = if is_weekly_single {
                    ("wk", "Weekly", "wk", 604_800u64)
                } else {
                    ("5h", "5-Hour", "5h", 18_000u64)
                };
                let win = pri
                    .limit_window_seconds
                    .and_then(|s| u64::try_from(s).ok())
                    .filter(|&s| s > 0)
                    .or(Some(default_win));
                buckets.push(
                    UsageBucket::new(
                        id,
                        label,
                        short_label,
                        BucketLayout::InlinePair,
                        is_primary,
                        pct,
                        norm,
                    )
                    .with_window_seconds(win),
                );
            }
        }
    }

    if let Some(ref cr) = parsed.code_review_rate_limit {
        if let Some(ref pri) = cr.primary_window {
            if let Some(pct) = pri.used_percent {
                if pct > 0.0 {
                    let norm = pri.reset_at.and_then(|ts| reset::normalize_epoch(ts, tz));
                    let win = pri
                        .limit_window_seconds
                        .and_then(|s| u64::try_from(s).ok())
                        .filter(|&s| s > 0)
                        .or(Some(604_800));
                    buckets.push(
                        UsageBucket::new(
                            "code-review",
                            "Code Review",
                            "Review",
                            BucketLayout::InlinePair,
                            false,
                            pct,
                            norm,
                        )
                        .with_window_seconds(win),
                    );
                }
            }
        }
    }

    if buckets.is_empty() {
        return Err(anyhow!("No rate_limit windows present in Codex wham/usage"));
    }

    let credits = parse_codex_credits(parsed.credits.as_ref());

    Ok(ParsedCodexUsage {
        email: parsed.email.filter(|s| !s.trim().is_empty()),
        plan_tier: parsed.plan_type.as_deref().and_then(normalize_codex_plan),
        buckets,
        credits,
    })
}

fn parse_codex_credits(raw: Option<&WhamCredits>) -> Option<CreditSummary> {
    let c = raw?;
    if !c.has_credits {
        return None;
    }
    let bal = c.balance.as_ref().and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    })?;
    Some(CreditSummary {
        label: "Credits".to_string(),
        used_dollars: None,
        limit_dollars: None,
        balance_dollars: Some(bal),
        used_percent: None,
    })
}

// ---------------------------------------------------------------------------
// Tier 1B: `codex app-server` JSON-RPC (`account/rateLimits/read`)
// ---------------------------------------------------------------------------

fn fetch_via_codex_app_server(
    codex_bin: &Path,
    profile_path: Option<&Path>,
    ctx: &FetchContext,
) -> Result<ParsedCodexUsage> {
    let reqs = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"gnome-agent-status\",\"version\":\"0.2.0\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"initialized\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"account/rateLimits/read\",\"params\":{}}\n"
    );

    let mut spec = CommandSpec::new(codex_bin, ctx.timeout)
        .args(["app-server"])
        .stdin_bytes(reqs.as_bytes());

    if let Some(p) = profile_path {
        let dir = if p.is_file() {
            p.parent().unwrap_or(p)
        } else {
            p
        };
        spec = spec.env("CODEX_HOME", dir.to_string_lossy().into_owned());
    }

    let line = run_jsonrpc_line_query(&spec, 2)?;
    parse_codex_app_server_response(&line, &Local)
}

pub fn parse_codex_app_server_response<T: TimeZone>(
    raw_line: &str,
    tz: &T,
) -> Result<ParsedCodexUsage>
where
    T::Offset: std::fmt::Display,
{
    let val: serde_json::Value =
        serde_json::from_str(raw_line).context("Invalid Codex app-server JSON-RPC response")?;
    if let Some(err) = val.get("error") {
        return Err(anyhow!("Codex app-server RPC error: {}", err));
    }
    let result = val
        .get("result")
        .ok_or_else(|| anyhow!("Missing 'result' in Codex app-server response"))?;

    let rate_limits = result
        .get("rateLimits")
        .or_else(|| result.pointer("/rateLimitsByLimitId/codex"))
        .ok_or_else(|| anyhow!("Missing 'rateLimits' in Codex app-server response"))?;

    let mut buckets = Vec::new();
    if let Some(sec) = rate_limits.get("secondary") {
        if let Some(pct) = sec.get("usedPercent").and_then(|v| v.as_f64()) {
            let norm = sec
                .get("resetsAt")
                .and_then(|v| v.as_i64())
                .and_then(|ts| reset::normalize_epoch(ts, tz));
            buckets.push(UsageBucket::new(
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
    if let Some(pri) = rate_limits.get("primary") {
        if let Some(pct) = pri.get("usedPercent").and_then(|v| v.as_f64()) {
            let norm = pri
                .get("resetsAt")
                .and_then(|v| v.as_i64())
                .and_then(|ts| reset::normalize_epoch(ts, tz));
            let is_primary = buckets.is_empty();
            let is_weekly_single = buckets.is_empty()
                && pri
                    .get("windowDurationMins")
                    .and_then(|v| v.as_i64())
                    .is_some_and(|m| m >= 7200);
            let (id, label, short_label) = if is_weekly_single {
                ("wk", "Weekly", "wk")
            } else {
                ("5h", "5-Hour", "5h")
            };
            buckets.push(UsageBucket::new(
                id,
                label,
                short_label,
                BucketLayout::InlinePair,
                is_primary,
                pct,
                norm,
            ));
        }
    }

    if buckets.is_empty() {
        return Err(anyhow!("No active windows in Codex app-server rateLimits"));
    }

    let plan_tier = result
        .get("planType")
        .and_then(|v| v.as_str())
        .and_then(normalize_codex_plan);

    let credits = result.get("credits").and_then(|c| {
        let has = c
            .get("hasCredits")
            .or_else(|| c.get("has_credits"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !has {
            return None;
        }
        let bal = c.get("balance").and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
        })?;
        Some(CreditSummary {
            label: "Credits".to_string(),
            used_dollars: None,
            limit_dollars: None,
            balance_dollars: Some(bal),
            used_percent: None,
        })
    });

    Ok(ParsedCodexUsage {
        email: None,
        plan_tier,
        buckets,
        credits,
    })
}

// ---------------------------------------------------------------------------
// Tier 2: Offline Session Rollout JSONL Scanner
// ---------------------------------------------------------------------------

pub fn scan_codex_rollout_sessions<T: TimeZone>(
    profile_path: &Path,
    now: DateTime<Utc>,
    tz: &T,
) -> Option<Vec<UsageBucket>>
where
    T::Offset: std::fmt::Display,
{
    let base_dir = if profile_path.is_file() {
        profile_path.parent()?
    } else {
        profile_path
    };
    let sessions_dir = base_dir.join("sessions");
    if !sessions_dir.is_dir() {
        return None;
    }

    let mut rollout_files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    collect_rollout_files(&sessions_dir, &mut rollout_files, 0);
    rollout_files.sort_by_key(|a| std::cmp::Reverse(a.0));

    for (_, file_path) in rollout_files.into_iter().take(5) {
        let Some(raw) = read_rollout_token_count_lines(&file_path) else {
            continue;
        };
        if let Some(buckets) = parse_codex_rollout_jsonl(&raw, now, tz) {
            return Some(buckets);
        }
    }
    None
}

fn read_rollout_token_count_lines(file_path: &Path) -> Option<String> {
    let is_zst = file_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zst"));

    if !is_zst {
        return fs::read_to_string(file_path).ok();
    }

    let file = fs::File::open(file_path).ok()?;
    let decoder = ruzstd::decoding::StreamingDecoder::new(file).ok()?;
    let reader = BufReader::new(decoder.take(64 * 1024 * 1024));

    let mut matched = String::new();
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        if line.contains("token_count") {
            matched.push_str(&line);
            matched.push('\n');
        }
    }
    if matched.is_empty() {
        None
    } else {
        Some(matched)
    }
}

fn collect_rollout_files(
    dir: &Path,
    out: &mut Vec<(std::time::SystemTime, PathBuf)>,
    depth: usize,
) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let p = entry.path();
        if p.is_dir() {
            collect_rollout_files(&p, out, depth + 1);
        } else if p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".jsonl") || n.ends_with(".jsonl.zst"))
        {
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            out.push((mtime, p));
        }
    }
}

pub fn parse_codex_rollout_jsonl<T: TimeZone>(
    raw: &str,
    now: DateTime<Utc>,
    tz: &T,
) -> Option<Vec<UsageBucket>>
where
    T::Offset: std::fmt::Display,
{
    let now_epoch = now.timestamp();
    for line in raw.lines().rev() {
        let trimmed = line.trim();
        if !trimmed.contains("token_count") {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if val.get("type").and_then(|v| v.as_str()) != Some("event_msg") {
            continue;
        }
        let Some(payload) = val.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(|v| v.as_str()) != Some("token_count") {
            continue;
        }
        let Some(rate_limits) = payload.get("rate_limits").filter(|v| !v.is_null()) else {
            continue;
        };

        let event_epoch = val
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp())
            .unwrap_or(now_epoch);

        let mut buckets = Vec::new();
        if let Some(sec) = rate_limits.get("secondary").filter(|v| !v.is_null()) {
            if let Some(pct) = sec.get("used_percent").and_then(|v| v.as_f64()) {
                let reset_epoch = sec
                    .get("resets_at")
                    .and_then(|v| v.as_i64())
                    .or_else(|| {
                        sec.get("resets_in_seconds")
                            .and_then(|v| v.as_i64())
                            .map(|rel| event_epoch + rel)
                    });
                let effective_pct = if reset_epoch.is_some_and(|r| now_epoch >= r) {
                    0.0
                } else {
                    pct
                };
                let norm = reset_epoch.and_then(|ts| reset::normalize_epoch(ts, tz));
                buckets.push(UsageBucket::new(
                    "wk",
                    "Weekly",
                    "wk",
                    BucketLayout::InlinePair,
                    true,
                    effective_pct,
                    norm,
                ));
            }
        }
        if let Some(pri) = rate_limits.get("primary").filter(|v| !v.is_null()) {
            if let Some(pct) = pri.get("used_percent").and_then(|v| v.as_f64()) {
                let reset_epoch = pri
                    .get("resets_at")
                    .and_then(|v| v.as_i64())
                    .or_else(|| {
                        pri.get("resets_in_seconds")
                            .and_then(|v| v.as_i64())
                            .map(|rel| event_epoch + rel)
                    });
                let effective_pct = if reset_epoch.is_some_and(|r| now_epoch >= r) {
                    0.0
                } else {
                    pct
                };
                let norm = reset_epoch.and_then(|ts| reset::normalize_epoch(ts, tz));
                let is_primary = buckets.is_empty();
                let is_weekly_single = buckets.is_empty()
                    && pri
                        .get("window_minutes")
                        .and_then(|v| v.as_i64())
                        .is_some_and(|m| m >= 7200);
                let (id, label, short_label) = if is_weekly_single {
                    ("wk", "Weekly", "wk")
                } else {
                    ("5h", "5-Hour", "5h")
                };
                buckets.push(UsageBucket::new(
                    id,
                    label,
                    short_label,
                    BucketLayout::InlinePair,
                    is_primary,
                    effective_pct,
                    norm,
                ));
            }
        }

        if !buckets.is_empty() {
            return Some(buckets);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_codex_wham_usage_and_credits() {
        let raw = r#"{
          "user_id": "user-1",
          "account_id": "acct-1",
          "email": "dev@example.com",
          "plan_type": "pro",
          "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {
              "used_percent": 14,
              "limit_window_seconds": 18000,
              "reset_after_seconds": 14279,
              "reset_at": 1794371186
            },
            "secondary_window": {
              "used_percent": 35,
              "limit_window_seconds": 604800,
              "reset_after_seconds": 584279,
              "reset_at": 1794941186
            }
          },
          "credits": {
            "has_credits": true,
            "unlimited": false,
            "balance": "42.50"
          }
        }"#;

        let parsed = parse_codex_wham_usage(raw, &Utc).expect("parse wham");
        assert_eq!(parsed.email.as_deref(), Some("dev@example.com"));
        assert_eq!(parsed.plan_tier.as_deref(), Some("Pro"));
        assert_eq!(parsed.buckets.len(), 2);
        assert_eq!(parsed.buckets[0].id, "wk");
        assert!(parsed.buckets[0].is_primary);
        assert_eq!(parsed.buckets[0].used_percent, 35);
        assert_eq!(parsed.buckets[1].id, "5h");
        assert_eq!(parsed.buckets[1].used_percent, 14);
        assert_eq!(parsed.credits.unwrap().balance_dollars, Some(42.50));
    }

    #[test]
    fn test_parse_codex_app_server_and_rollout_jsonl() {
        let rpc_line = r#"{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":19,"windowDurationMins":300,"resetsAt":1794371186},"secondary":{"usedPercent":44,"windowDurationMins":10080,"resetsAt":1794941186}},"planType":"plus","credits":{"hasCredits":false,"balance":"0"}}}"#;
        let parsed = parse_codex_app_server_response(rpc_line, &Utc).expect("parse app-server");
        assert_eq!(parsed.plan_tier.as_deref(), Some("Plus"));
        assert_eq!(parsed.buckets[0].used_percent, 44);
        assert_eq!(parsed.buckets[1].used_percent, 19);
        assert!(parsed.credits.is_none());

        let jsonl = concat!(
            "{\"timestamp\":\"2026-11-10T20:00:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"rate_limits\":{\"primary\":{\"used_percent\":14.0,\"resets_in_seconds\":3600},\"secondary\":{\"used_percent\":35.0,\"resets_in_seconds\":500000}}}}\n",
            "{\"timestamp\":\"2026-11-10T20:05:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"rate_limits\":null}}\n"
        );
        // At 21:30 UTC (past the 1h primary reset, before the weekly reset), primary decays to 0%!
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 21, 30, 0).unwrap();
        let buckets = parse_codex_rollout_jsonl(jsonl, now, &Utc).expect("scan rollout jsonl");
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].id, "wk");
        assert_eq!(buckets[0].used_percent, 35);
        assert_eq!(buckets[1].id, "5h");
        assert_eq!(buckets[1].used_percent, 0);
    }

    #[test]
    fn test_pro_single_weekly_window_and_zstd_rollout_scanner() {
        let pro_wham = r#"{
          "plan_type": "pro",
          "rate_limit": {
            "primary_window": {
              "used_percent": 19,
              "limit_window_seconds": 604800,
              "reset_at": 1794860654
            },
            "secondary_window": null
          }
        }"#;
        let parsed = parse_codex_wham_usage(pro_wham, &Utc).expect("parse Pro single-window wham");
        assert_eq!(parsed.buckets.len(), 1);
        assert_eq!(parsed.buckets[0].id, "wk");
        assert_eq!(parsed.buckets[0].label, "Weekly");
        assert!(parsed.buckets[0].is_primary);
        assert_eq!(parsed.buckets[0].used_percent, 19);

        // Zstandard `.jsonl.zst` session rollout file with `window_minutes: 10080` and `resets_at`
        let tmp_dir = std::env::temp_dir().join(format!(
            "gas-codex-zst-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let day_dir = tmp_dir.join("sessions/2026/11/10");
        fs::create_dir_all(&day_dir).unwrap();

        let rollout_line = "{\"timestamp\":\"2026-11-10T20:00:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"rate_limits\":{\"primary\":{\"used_percent\":23.0,\"window_minutes\":10080,\"resets_at\":1794860654},\"secondary\":null}}}\n";
        let compressed = ruzstd::encoding::compress_to_vec(
            rollout_line.as_bytes(),
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        fs::write(day_dir.join("rollout-test.jsonl.zst"), compressed).unwrap();

        let now = Utc.with_ymd_and_hms(2026, 11, 10, 21, 0, 0).unwrap();
        let scanned = scan_codex_rollout_sessions(&tmp_dir, now, &Utc)
            .expect("scan .jsonl.zst rollout session");
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].id, "wk");
        assert_eq!(scanned[0].used_percent, 23);
        assert_eq!(scanned[0].resets_at_epoch, Some(1794860654));

        let _ = fs::remove_dir_all(&tmp_dir);
    }
}
