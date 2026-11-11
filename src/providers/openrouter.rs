//! OpenRouter (`openrouter`) usage provider:
//! - Discovers manual `account.path` (raw API key or JSON `{"api_key": "..."}` / `{"key": "..."}`),
//!   or auto-discovers `$OPENROUTER_API_KEY` and `~/.config/openrouter/{api_key,config.json,auth.json}`,
//!   omitting cleanly when unconfigured.
//! - Queries `GET https://openrouter.ai/api/v1/key` (`Authorization: Bearer <key>`).
//! - When `limit > 0` is configured on the key: surfaces primary window/limit bucket and `credits`.
//! - When `limit` is `null` (unlimited key): queries `GET /api/v1/credits` for prepaid pool balance
//!   when available, or emits a `0%` `"Pay-as-you-go"` bucket with spent dollar summary.

use crate::cache::CachedAccountState;
use crate::config::{
    abbreviate_home, expand_tilde_with_home, slugify_id, ManualAccountConfig, PollMode,
};
use crate::model::{AccountReport, AccountState, BucketLayout, CreditSummary, UsageBucket};
use crate::providers::{AccountDescriptor, AccountFetchOutcome, FetchContext, ProviderMeta, UsageProvider};
use anyhow::{anyhow, Context, Result};
use chrono::{Datelike, Duration as ChronoDuration, TimeZone, Utc};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_OPENROUTER_KEY_URL: &str = "https://openrouter.ai/api/v1/key";
const DEFAULT_OPENROUTER_CREDITS_URL: &str = "https://openrouter.ai/api/v1/credits";
const ENV_SOURCE_LABEL: &str = "$OPENROUTER_API_KEY";

pub struct OpenRouterProvider;

impl OpenRouterProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OpenRouterProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageProvider for OpenRouterProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            id: "openrouter",
            display_name: "OpenRouter",
            icon_name: "provider-openrouter",
            dashboard_url: "https://openrouter.ai/activity",
            default_ttl_secs: 300,
        }
    }

    fn discover_accounts(
        &self,
        manual: &[ManualAccountConfig],
        auto_discover: bool,
        ctx: &FetchContext,
    ) -> Vec<AccountDescriptor> {
        let home = ctx.home_dir.as_deref();
        let mut out: Vec<AccountDescriptor> = Vec::new();

        for (idx, m) in manual.iter().enumerate() {
            let expanded = m
                .path
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(|p| expand_tilde_with_home(p, home));
            let source_label = expanded
                .as_deref()
                .map(|p| abbreviate_home(p, home))
                .or_else(|| {
                    ctx.env_var("OPENROUTER_API_KEY")
                        .map(|_| ENV_SOURCE_LABEL.to_string())
                });
            let dedup_key = expanded
                .as_deref()
                .and_then(|p| read_openrouter_api_key_from_path(p).ok())
                .map(|k| fingerprint_secret(&k));

            let id_slug = if m.id.trim().is_empty() {
                format!("manual-{}", idx + 1)
            } else {
                slugify_id(&m.id)
            };

            out.push(AccountDescriptor {
                provider_id: "openrouter".to_string(),
                id: format!("openrouter:{id_slug}"),
                label: sanitize_openrouter_label(Some(&m.display_label()), None),
                dedup_key,
                path: expanded,
                source_label,
                binary: m.binary.clone(),
                is_system_default: idx == 0 && !auto_discover,
                poll_mode: m.poll_mode,
                login_command: Some("https://openrouter.ai/settings/keys".to_string()),
                switch_command: m.switch_command.clone(),
            });
        }

        if auto_discover {
            if let Some(env_key) = ctx.env_var("OPENROUTER_API_KEY") {
                out.push(AccountDescriptor {
                    provider_id: "openrouter".to_string(),
                    id: "openrouter:env".to_string(),
                    label: "OpenRouter".to_string(),
                    dedup_key: Some(fingerprint_secret(&env_key)),
                    path: None,
                    source_label: Some(ENV_SOURCE_LABEL.to_string()),
                    binary: None,
                    is_system_default: true,
                    poll_mode: PollMode::Active,
                    login_command: Some("https://openrouter.ai/settings/keys".to_string()),
                    switch_command: None,
                });
            }

            let has_env_default = out.iter().any(|a| a.is_system_default);
            for (idx, file_path) in candidate_openrouter_files(home, ctx.xdg_config_dir.as_deref())
                .into_iter()
                .enumerate()
            {
                if let Ok(file_key) = read_openrouter_api_key_from_path(&file_path) {
                    let slug = if idx == 0 && !has_env_default {
                        "default".to_string()
                    } else {
                        format!("config-{}", idx + 1)
                    };
                    out.push(AccountDescriptor {
                        provider_id: "openrouter".to_string(),
                        id: format!("openrouter:{slug}"),
                        label: "OpenRouter".to_string(),
                        dedup_key: Some(fingerprint_secret(&file_key)),
                        source_label: Some(abbreviate_home(&file_path, home)),
                        path: Some(file_path),
                        binary: None,
                        is_system_default: idx == 0 && !has_env_default,
                        poll_mode: PollMode::Active,
                        login_command: Some("https://openrouter.ai/settings/keys".to_string()),
                        switch_command: None,
                    });
                    break;
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
        let is_auto = account.id == "openrouter:env"
            || account.id == "openrouter:default"
            || account.id.starts_with("openrouter:config-");

        let api_key = match resolve_account_key(account, ctx) {
            Ok(k) => k,
            Err(err) => {
                if is_auto {
                    return AccountFetchOutcome::Omit;
                }
                return AccountFetchOutcome::SignedOut {
                    message: format!("{err:#}"),
                    login_command: account.login_command.clone(),
                    label: Some(account.label.clone()),
                    plan_tier: None,
                };
            }
        };

        let key_url = ctx
            .endpoints
            .openrouter_key_url
            .as_deref()
            .unwrap_or(DEFAULT_OPENROUTER_KEY_URL);
        let auth_header = format!("Bearer {api_key}");
        let headers = [
            ("Authorization", auth_header.as_str()),
            ("Accept", "application/json"),
        ];

        let key_resp = match ctx.http.get(key_url, &headers) {
            Ok(resp) => resp,
            Err(err) => {
                return AccountFetchOutcome::Error {
                    message: format!("{err:#}"),
                };
            }
        };

        if key_resp.status == 429 {
            return AccountFetchOutcome::RateLimited {
                retry_after_secs: key_resp.retry_after,
            };
        }
        if key_resp.status == 401 || key_resp.status == 403 {
            return AccountFetchOutcome::SignedOut {
                message: format!("OpenRouter API returned HTTP {}", key_resp.status),
                login_command: account.login_command.clone(),
                label: Some(account.label.clone()),
                plan_tier: None,
            };
        }
        if !key_resp.is_success() {
            return AccountFetchOutcome::Error {
                message: format!("OpenRouter API returned HTTP {}", key_resp.status),
            };
        }

        let parsed_key = match parse_openrouter_key_json(&key_resp.body) {
            Ok(p) => p,
            Err(err) => {
                return AccountFetchOutcome::Error {
                    message: format!("{err:#}"),
                };
            }
        };

        // When `limit` is absent/unlimited, optionally query `/api/v1/credits` for prepaid pool balance.
        let credits_payload = if parsed_key.limit.is_none_or(|l| l <= 0.0) {
            let credits_url = resolve_credits_url(ctx);
            ctx.http
                .get(&credits_url, &headers)
                .ok()
                .filter(|r| r.is_success())
                .and_then(|r| parse_openrouter_credits_json(&r.body).ok())
        } else {
            None
        };

        let built = build_openrouter_report(account, &parsed_key, credits_payload.as_ref(), ctx.now);
        AccountFetchOutcome::Success(built)
    }
}

fn resolve_credits_url(ctx: &FetchContext) -> String {
    if let Some(ref explicit) = ctx.endpoints.openrouter_credits_url {
        return explicit.clone();
    }
    if let Some(ref key_override) = ctx.endpoints.openrouter_key_url {
        if let Some(prefix) = key_override.strip_suffix("/api/v1/key") {
            return format!("{prefix}/api/v1/credits");
        }
    }
    DEFAULT_OPENROUTER_CREDITS_URL.to_string()
}

fn candidate_openrouter_files(home: Option<&Path>, xdg_config: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(xdg) = xdg_config {
        dirs.push(xdg.join("openrouter"));
    }
    if let Some(h) = home {
        let def = h.join(".config").join("openrouter");
        if !dirs.contains(&def) {
            dirs.push(def);
        }
    }

    let mut files = Vec::new();
    for dir in dirs {
        for fname in ["api_key", "config.json", "auth.json"] {
            let p = dir.join(fname);
            if p.is_file() && !files.contains(&p) {
                files.push(p);
            }
        }
    }
    files
}

fn resolve_account_key(account: &AccountDescriptor, ctx: &FetchContext) -> Result<String> {
    if account.source_label.as_deref() == Some(ENV_SOURCE_LABEL) {
        if let Some(k) = ctx.env_var("OPENROUTER_API_KEY") {
            return Ok(k);
        }
    }
    if let Some(ref path) = account.path {
        return read_openrouter_api_key_from_path(path);
    }
    if let Some(k) = ctx.env_var("OPENROUTER_API_KEY") {
        return Ok(k);
    }
    Err(anyhow!("OpenRouter API key not found"))
}

pub fn read_openrouter_api_key_from_path(path: &Path) -> Result<String> {
    if path.is_dir() {
        for fname in ["api_key", "config.json", "auth.json"] {
            let candidate = path.join(fname);
            if candidate.is_file() {
                if let Ok(k) = read_key_file(&candidate) {
                    return Ok(k);
                }
            }
        }
        return Err(anyhow!(
            "No OpenRouter API key file found in {}",
            path.display()
        ));
    }

    read_key_file(path)
}

fn read_key_file(file_path: &Path) -> Result<String> {
    let raw = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read {}", file_path.display()))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("API key file {} is empty", file_path.display()));
    }

    if trimmed.starts_with('{') {
        let val: serde_json::Value = serde_json::from_str(trimmed)
            .with_context(|| format!("Invalid JSON in {}", file_path.display()))?;
        for field in ["api_key", "key", "openrouter_api_key", "apiKey", "token"] {
            if let Some(s) = val
                .get(field)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Ok(s.to_string());
            }
        }
        return Err(anyhow!(
            "No 'api_key' or 'key' field found in {}",
            file_path.display()
        ));
    }

    let first_line = trimmed
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .ok_or_else(|| anyhow!("No API key found in {}", file_path.display()))?;
    Ok(first_line.to_string())
}

fn fingerprint_secret(secret: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(secret.trim().as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(12);
    for b in &digest[..6] {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{b:02x}");
    }
    format!("openrouter-key:{hex}")
}

fn sanitize_openrouter_label(custom_label: Option<&str>, api_label: Option<&str>) -> String {
    if let Some(custom) = custom_label.map(str::trim).filter(|s| !s.is_empty()) {
        if custom != "OpenRouter"
            && custom != "default"
            && custom != "env"
            && !looks_like_secret_label(custom)
        {
            return custom.to_string();
        }
    }
    if let Some(lbl) = api_label.map(str::trim).filter(|s| !s.is_empty()) {
        if !looks_like_secret_label(lbl) {
            return lbl.to_string();
        }
    }
    "OpenRouter".to_string()
}

fn looks_like_secret_label(s: &str) -> bool {
    let lower = s.trim().to_ascii_lowercase();
    lower.starts_with("sk-")
        || lower.contains("sk-or-")
        || crate::http::redact_secrets(s).contains("[REDACTED]")
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenRouterKeyData {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub usage: Option<f64>,
    #[serde(default)]
    pub usage_daily: Option<f64>,
    #[serde(default)]
    pub usage_weekly: Option<f64>,
    #[serde(default)]
    pub usage_monthly: Option<f64>,
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default)]
    pub limit_remaining: Option<f64>,
    #[serde(default)]
    pub limit_reset: Option<String>,
    #[serde(default)]
    pub is_free_tier: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterKeyEnvelope {
    data: OpenRouterKeyData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenRouterCreditsData {
    #[serde(default)]
    pub total_credits: f64,
    #[serde(default)]
    pub total_usage: f64,
}

#[derive(Debug, Deserialize)]
struct OpenRouterCreditsEnvelope {
    data: OpenRouterCreditsData,
}

pub fn parse_openrouter_key_json(raw: &str) -> Result<OpenRouterKeyData> {
    let env: OpenRouterKeyEnvelope =
        serde_json::from_str(raw).context("Failed to parse OpenRouter /api/v1/key JSON")?;
    Ok(env.data)
}

pub fn parse_openrouter_credits_json(raw: &str) -> Result<OpenRouterCreditsData> {
    let env: OpenRouterCreditsEnvelope =
        serde_json::from_str(raw).context("Failed to parse OpenRouter /api/v1/credits JSON")?;
    Ok(env.data)
}

fn compute_openrouter_reset(
    limit_reset: Option<&str>,
    now: chrono::DateTime<Utc>,
) -> Option<(i64, String)> {
    let kind = limit_reset?.trim().to_ascii_lowercase();
    let today_start = Utc
        .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
        .single()?;
    let target = match kind.as_str() {
        "daily" => today_start + ChronoDuration::days(1),
        "weekly" => {
            let days_until_monday =
                (7 - now.weekday().num_days_from_monday() as i64).rem_euclid(7);
            let delta = if days_until_monday == 0 {
                7
            } else {
                days_until_monday
            };
            today_start + ChronoDuration::days(delta)
        }
        "monthly" => {
            let (ny, nm) = if now.month() == 12 {
                (now.year() + 1, 1)
            } else {
                (now.year(), now.month() + 1)
            };
            Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0).single()?
        }
        _ => return None,
    };
    Some((target.timestamp(), target.to_rfc3339()))
}

pub fn build_openrouter_report(
    account: &AccountDescriptor,
    key_data: &OpenRouterKeyData,
    credits_data: Option<&OpenRouterCreditsData>,
    now: chrono::DateTime<Utc>,
) -> AccountReport {
    let label = sanitize_openrouter_label(Some(&account.label), key_data.label.as_deref());
    let usage = key_data.usage.unwrap_or(0.0).max(0.0);
    let is_free = key_data.is_free_tier.unwrap_or(false);

    if let Some(limit) = key_data.limit.filter(|&l| l > 0.0 && l.is_finite()) {
        let used_pct_f64 = ((usage / limit) * 100.0).clamp(0.0, 100.0);
        let reset = compute_openrouter_reset(key_data.limit_reset.as_deref(), now);
        let (bucket_id, bucket_label, short_label) = match key_data
            .limit_reset
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("daily") => ("24h", "Daily Limit", "24h"),
            Some("weekly") => ("wk", "Weekly Limit", "wk"),
            Some("monthly") => ("31d", "Monthly Limit", "31d"),
            _ => ("limit", "Key Limit", "limit"),
        };

        let balance = key_data
            .limit_remaining
            .map(|r| r.max(0.0))
            .or_else(|| Some((limit - usage).max(0.0)));
        let bucket = UsageBucket::new(
            bucket_id,
            bucket_label,
            short_label,
            BucketLayout::FullRow,
            true,
            used_pct_f64,
            reset,
        )
        .with_extra_info(format!("${usage:.2} / ${limit:.2} used"));

        let used_pct_u8 = bucket.used_percent;
        let plan_tier = if is_free {
            Some("Free".to_string())
        } else {
            Some("Capped".to_string())
        };

        return AccountReport {
            id: account.id.clone(),
            label,
            source_label: account.source_label.clone(),
            is_system_default: account.is_system_default,
            is_active: false,
            plan_tier,
            state: AccountState::ActiveOk,
            is_stale: false,
            status_note: None,
            extra_info: Some(format!("${usage:.2} / ${limit:.2} key limit used")),
            error: None,
            login_command: account.login_command.clone(),
            updated_at_epoch: Some(now.timestamp()),
            buckets: vec![bucket],
            credits: Some(CreditSummary {
                label: "Key Limit".to_string(),
                used_dollars: Some(usage),
                limit_dollars: Some(limit),
                balance_dollars: balance,
                used_percent: Some(used_pct_u8),
            }),
            sparkline: Vec::new(),
        };
    }

    if let Some(cd) = credits_data.filter(|c| c.total_credits > 0.0 && c.total_credits.is_finite())
    {
        let total_credits = cd.total_credits.max(0.0);
        let total_usage = cd.total_usage.max(0.0);
        let balance = (total_credits - total_usage).max(0.0);
        let used_pct_f64 = ((total_usage / total_credits) * 100.0).clamp(0.0, 100.0);

        let bucket = UsageBucket::new(
            "credits",
            "Prepaid Credits",
            "credits",
            BucketLayout::FullRow,
            true,
            used_pct_f64,
            None,
        )
        .with_extra_info(format!(
            "${total_usage:.2} / ${total_credits:.2} credits used (${balance:.2} left)"
        ));
        let used_pct_u8 = bucket.used_percent;

        return AccountReport {
            id: account.id.clone(),
            label,
            source_label: account.source_label.clone(),
            is_system_default: account.is_system_default,
            is_active: false,
            plan_tier: Some(if is_free { "Free" } else { "Credits" }.to_string()),
            state: AccountState::ActiveOk,
            is_stale: false,
            status_note: None,
            extra_info: Some(format!("${balance:.2} remaining of ${total_credits:.2}")),
            error: None,
            login_command: account.login_command.clone(),
            updated_at_epoch: Some(now.timestamp()),
            buckets: vec![bucket],
            credits: Some(CreditSummary {
                label: "Prepaid Credits".to_string(),
                used_dollars: Some(total_usage),
                limit_dollars: Some(total_credits),
                balance_dollars: Some(balance),
                used_percent: Some(used_pct_u8),
            }),
            sparkline: Vec::new(),
        };
    }

    // Unlimited pay-as-you-go fallback
    let note = format!("Pay-as-you-go · ${usage:.2} spent");
    let bucket = UsageBucket::new(
        "payg",
        "Pay-as-you-go",
        "payg",
        BucketLayout::FullRow,
        true,
        0.0,
        None,
    )
    .with_extra_info(note.clone());

    AccountReport {
        id: account.id.clone(),
        label,
        source_label: account.source_label.clone(),
        is_system_default: account.is_system_default,
        is_active: false,
        plan_tier: Some(if is_free { "Free" } else { "Pay-as-you-go" }.to_string()),
        state: AccountState::ActiveOk,
        is_stale: false,
        status_note: Some("Pay-as-you-go".to_string()),
        extra_info: Some(note),
        error: None,
        login_command: account.login_command.clone(),
        updated_at_epoch: Some(now.timestamp()),
        buckets: vec![bucket],
        credits: Some(CreditSummary {
            label: "Pay-as-you-go".to_string(),
            used_dollars: Some(usage),
            limit_dollars: None,
            balance_dollars: None,
            used_percent: None,
        }),
        sparkline: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{EndpointOverrides, HttpClient};
    use crate::providers::deduplicate_accounts;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_openrouter_limit_windows_prepaid_credits_and_payg_fallback() {
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 16, 0, 0).unwrap();
        let masked_secret_label = format!("{}abcd...9999", concat!("sk-", "or-v1-"));
        let desc = AccountDescriptor {
            provider_id: "openrouter".to_string(),
            id: "openrouter:default".to_string(),
            label: "OpenRouter".to_string(),
            dedup_key: None,
            path: None,
            source_label: Some("~/.config/openrouter/api_key".to_string()),
            binary: None,
            is_system_default: true,
            poll_mode: PollMode::Active,
            login_command: None,
            switch_command: None,
        };

        // 1. Weekly capped key (`limit > 0`, `limit_reset == "weekly"`) + secret-shaped API label sanitized!
        let capped_json = format!(
            r#"{{
                "data": {{
                    "label": "{masked_secret_label}",
                    "usage": 17.5,
                    "usage_daily": 4.0,
                    "usage_weekly": 17.5,
                    "usage_monthly": 42.0,
                    "limit": 50.0,
                    "limit_remaining": 32.5,
                    "limit_reset": "weekly",
                    "is_free_tier": false
                }}
            }}"#
        );
        let key_data = parse_openrouter_key_json(&capped_json).unwrap();
        let rep_capped = build_openrouter_report(&desc, &key_data, None, now);
        assert_eq!(rep_capped.label, "OpenRouter");
        assert!(!rep_capped.label.contains("abcd"));
        assert_eq!(rep_capped.buckets.len(), 1);
        assert_eq!(rep_capped.buckets[0].id, "wk");
        assert_eq!(rep_capped.buckets[0].used_percent, 35);
        assert_eq!(rep_capped.buckets[0].window_seconds, Some(604_800));
        assert!(rep_capped.buckets[0].resets_at_epoch.is_some());
        let cred = rep_capped.credits.unwrap();
        assert_eq!(cred.used_dollars, Some(17.5));
        assert_eq!(cred.limit_dollars, Some(50.0));
        assert_eq!(cred.balance_dollars, Some(32.5));

        // 2. Unlimited key (`limit: null`) + `/api/v1/credits` (`total_credits > 0`)
        let unlim_json = r#"{
            "data": {
                "label": "Production Router",
                "usage": 62.0,
                "limit": null,
                "limit_remaining": null,
                "limit_reset": null,
                "is_free_tier": false
            }
        }"#;
        let credits_json = r#"{"data": {"total_credits": 100.0, "total_usage": 62.4}}"#;
        let unlim_key = parse_openrouter_key_json(unlim_json).unwrap();
        let credits = parse_openrouter_credits_json(credits_json).unwrap();
        let rep_credits = build_openrouter_report(&desc, &unlim_key, Some(&credits), now);
        assert_eq!(rep_credits.label, "Production Router");
        assert_eq!(rep_credits.buckets[0].id, "credits");
        assert_eq!(rep_credits.buckets[0].label, "Prepaid Credits");
        assert_eq!(rep_credits.buckets[0].used_percent, 62);
        assert_eq!(rep_credits.credits.unwrap().balance_dollars, Some(37.6));

        // 3. Unlimited key without prepaid credits -> 0% Pay-as-you-go
        let rep_payg = build_openrouter_report(&desc, &unlim_key, None, now);
        assert_eq!(rep_payg.buckets[0].used_percent, 0);
        assert_eq!(rep_payg.status_note.as_deref(), Some("Pay-as-you-go"));
        assert_eq!(rep_payg.credits.unwrap().used_dollars, Some(62.0));
    }

    #[test]
    fn test_openrouter_discovery_dedup_omission_and_mock_http_fetch() {
        let tmp = std::env::temp_dir().join(format!(
            "gas-openrouter-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = tmp.join("home");
        let xdg = home.join(".config");
        fs::create_dir_all(&xdg).unwrap();

        let provider = OpenRouterProvider::new();

        // 1. Unconfigured -> noise-free empty discovery!
        let empty_ctx = FetchContext {
            http: HttpClient::new(Duration::from_secs(3)),
            timeout: Duration::from_secs(3),
            now: Utc.with_ymd_and_hms(2026, 11, 11, 16, 0, 0).unwrap(),
            home_dir: Some(home.clone()),
            xdg_config_dir: Some(xdg.clone()),
            proc_root: tmp.join("proc"),
            endpoints: EndpointOverrides::default(),
            env_overrides: Some(BTreeMap::new()),
        };
        assert!(provider.discover_accounts(&[], true, &empty_ctx).is_empty());

        // 2. Same key in `$OPENROUTER_API_KEY` and `~/.config/openrouter/config.json` deduplicates cleanly
        let test_secret = format!("{}unitTestSecretKey987654321", concat!("sk-", "or-v1-"));
        let cfg_file = xdg.join("openrouter/config.json");
        fs::create_dir_all(cfg_file.parent().unwrap()).unwrap();
        fs::write(&cfg_file, format!(r#"{{"api_key":"{test_secret}"}}"#)).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let expected_auth = format!("Bearer {test_secret}");
        let srv = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                assert!(req.contains(&expected_auth));
                let body = if req.starts_with("GET /api/v1/key ") {
                    r#"{"data":{"label":"Work Router","usage":12.0,"limit":null,"is_free_tier":false}}"#
                } else {
                    r#"{"data":{"total_credits":80.0,"total_usage":20.0}}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(resp.as_bytes()).unwrap();
            }
        });

        let mut env_map = BTreeMap::new();
        env_map.insert("OPENROUTER_API_KEY".to_string(), test_secret.clone());
        let ctx = FetchContext {
            endpoints: EndpointOverrides {
                openrouter_key_url: Some(format!("http://{addr}/api/v1/key")),
                openrouter_credits_url: None, // auto-derived from openrouter_key_url!
                ..Default::default()
            },
            env_overrides: Some(env_map),
            ..empty_ctx
        };

        let raw = provider.discover_accounts(&[], true, &ctx);
        let deduped = deduplicate_accounts(raw);
        assert_eq!(deduped.len(), 1);
        assert!(!deduped[0].dedup_key.as_deref().unwrap().contains(&test_secret));

        let outcome = provider.fetch_account(&deduped[0], None, &ctx);
        srv.join().unwrap();

        match outcome {
            AccountFetchOutcome::Success(rep) => {
                assert_eq!(rep.label, "Work Router");
                assert_eq!(rep.buckets[0].id, "credits");
                assert_eq!(rep.buckets[0].used_percent, 25);
                let serialized = serde_json::to_string(&rep).unwrap();
                assert!(!serialized.contains(&test_secret));
            }
            other => panic!("expected Success, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&tmp);
    }
}
