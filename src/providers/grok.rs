//! xAI Grok (`grok`) usage provider:
//! - Credential parser supporting `GROK_OAUTH_TOKEN`, nested `"https://auth.x.ai::<uuid>"` maps
//!   in `$GROK_HOME/auth.json` (plus `~/.grok-swap/accounts/*/auth.json` deduplication), and flat OAuth JSON.
//! - Tier 1: `GET https://cli-chat-proxy.grok.com/v1/billing?format=credits` with omitted-zero
//!   protobuf `creditUsagePercent` fix (`0.0` default when `currentPeriod` is present).
//! - Tier 2: Legacy `grok agent stdio` (`_x.ai/billing`) ACP fallback.

use crate::cache::CachedAccountState;
use crate::config::{
    abbreviate_home, discover_provider_locations, expand_tilde_with_home, resolve_binary_for,
    ManualAccountConfig, PollMode,
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
use std::env;
use std::fs;
use std::path::Path;

const DEFAULT_GROK_BILLING_URL: &str =
    "https://cli-chat-proxy.grok.com/v1/billing?format=credits";

pub struct GrokProvider;

impl Default for GrokProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GrokProvider {
    pub fn new() -> Self {
        Self
    }
}

impl UsageProvider for GrokProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            id: "grok",
            display_name: "Grok",
            icon_name: "provider-grok",
            dashboard_url: "https://grok.com",
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
        let default_grok_dir = home.map(|h| h.join(".grok"));

        for m in manual {
            let expanded = m
                .path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|p| expand_tilde_with_home(p, home));

            let is_sys_default = match (&expanded, &default_grok_dir) {
                (Some(p), Some(def)) => p == def,
                (None, _) => true,
                _ => false,
            };

            let auth = expanded.as_deref().and_then(read_grok_auth_from_path);
            let source_label = expanded.as_deref().map(|p| abbreviate_home(p, home));
            let login_cmd = build_grok_login_command(
                expanded.as_deref(),
                source_label.as_deref(),
                is_sys_default,
            );

            let label = if m.label.as_deref().is_some_and(|l| !l.trim().is_empty()) {
                m.display_label()
            } else {
                auth.as_ref()
                    .and_then(|a| a.email.clone())
                    .unwrap_or_else(|| m.display_label())
            };

            let dedup_key = auth.and_then(|a| a.dedup_identity());

            out.push(AccountDescriptor {
                provider_id: "grok".to_string(),
                id: format!("grok:{}", m.id),
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
            if let Ok(env_tok) = env::var("GROK_OAUTH_TOKEN") {
                if !env_tok.trim().is_empty() {
                    let parsed = parse_grok_token_claims(env_tok.trim());
                    out.push(AccountDescriptor {
                        provider_id: "grok".to_string(),
                        id: "grok:env".to_string(),
                        label: parsed
                            .email
                            .clone()
                            .unwrap_or_else(|| "Grok (env)".to_string()),
                        dedup_key: parsed.dedup_identity(),
                        path: None,
                        source_label: Some("GROK_OAUTH_TOKEN".to_string()),
                        binary: None,
                        is_system_default: true,
                        poll_mode: PollMode::Active,
                        login_command: Some("grok login".to_string()),
                        switch_command: None,
                    });
                }
            }

            for loc in discover_provider_locations("grok", home, ctx.xdg_config_dir.as_deref()) {
                let auth = read_grok_auth_from_path(&loc.path);
                let email = auth.as_ref().and_then(|a| a.email.clone());
                let acct_id = if let Some(ref e) = email {
                    format!("grok:{}", e.to_ascii_lowercase())
                } else {
                    format!("grok:{}", loc.suggested_id)
                };
                let label = email.unwrap_or_else(|| {
                    if loc.is_system_default {
                        "Grok".to_string()
                    } else {
                        loc.suggested_id.clone()
                    }
                });
                let dedup_key = auth
                    .and_then(|a| a.dedup_identity())
                    .or_else(|| Some(loc.path.to_string_lossy().into_owned()));
                let login_cmd = build_grok_login_command(
                    Some(&loc.path),
                    Some(&loc.source_label),
                    loc.is_system_default,
                );

                out.push(AccountDescriptor {
                    provider_id: "grok".to_string(),
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
            .or_else(|| home.map(|h| h.join(".grok")));

        // Priority 1: `GROK_OAUTH_TOKEN` env var (when account has no isolated custom path or is env slot)
        let auth = if account.source_label.as_deref() == Some("GROK_OAUTH_TOKEN") {
            env::var("GROK_OAUTH_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(|t| parse_grok_token_claims(t.trim()))
        } else {
            profile_path
                .as_deref()
                .and_then(read_grok_auth_from_path)
                .or_else(|| {
                    if account.is_system_default {
                        env::var("GROK_OAUTH_TOKEN")
                            .ok()
                            .filter(|s| !s.trim().is_empty())
                            .map(|t| parse_grok_token_claims(t.trim()))
                    } else {
                        None
                    }
                })
        };

        let resolved_label = if account.label == "Grok" || account.label == "default" {
            auth.as_ref()
                .and_then(|a| a.email.clone())
                .unwrap_or_else(|| account.label.clone())
        } else {
            account.label.clone()
        };

        if let Some(ref creds) = auth {
            if !creds.access_token.is_empty() && !creds.is_expired(ctx.now) {
                let billing_url = ctx
                    .endpoints
                    .grok_billing_url
                    .as_deref()
                    .unwrap_or(DEFAULT_GROK_BILLING_URL);
                let bearer = format!("Bearer {}", creds.access_token);
                let headers = [
                    ("Authorization", bearer.as_str()),
                    ("x-xai-token-auth", "xai-grok-cli"),
                    ("x-grok-client-identifier", "grok-shell"),
                    ("User-Agent", "xai-grok-cli"),
                    ("Accept", "application/json"),
                ];

                match ctx.http.get(billing_url, &headers) {
                    Ok(resp) if resp.is_success() => {
                        match parse_grok_billing_json(&resp.body, ctx.now, &Local) {
                            Ok((buckets, credits)) => {
                                return AccountFetchOutcome::Success(AccountReport {
                                    id: account.id.clone(),
                                    label: resolved_label,
                                    source_label: account.source_label.clone(),
                                    is_system_default: account.is_system_default,
                                    is_active: false,
                                    plan_tier: None,
                                    state: AccountState::ActiveOk,
                                    is_stale: false,
                                    status_note: None,
                                    extra_info: None,
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
                        return AccountFetchOutcome::SignedOut {
                            message: format!("Grok billing endpoint returned HTTP {}", resp.status),
                            login_command: account.login_command.clone(),
                            label: Some(resolved_label),
                            plan_tier: None,
                        };
                    }
                    Ok(_) | Err(_) => {
                        // Fall through to legacy `grok agent stdio` ACP fallback if binary is present
                    }
                }
            }
        }

        // Tier 2: Legacy `grok agent stdio` (`_x.ai/billing`) ACP fallback
        let grok_bin = resolve_binary_for("grok", account.binary.as_deref());
        if grok_bin.is_file() || account.binary.is_some() {
            if let Ok((buckets, credits)) =
                fetch_via_grok_stdio(&grok_bin, profile_path.as_deref(), ctx)
            {
                return AccountFetchOutcome::Success(AccountReport {
                    id: account.id.clone(),
                    label: resolved_label,
                    source_label: account.source_label.clone(),
                    is_system_default: account.is_system_default,
                    is_active: false,
                    plan_tier: None,
                    state: AccountState::ActiveOk,
                    is_stale: false,
                    status_note: None,
                    extra_info: None,
                    error: None,
                    login_command: account.login_command.clone(),
                    updated_at_epoch: Some(ctx.now_epoch()),
                    buckets,
                    credits,
                });
            }
        }

        AccountFetchOutcome::SignedOut {
            message: "Grok OAuth credentials missing or expired".to_string(),
            login_command: account.login_command.clone(),
            label: Some(resolved_label),
            plan_tier: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Grok Auth Parsing (Nested `https://auth.x.ai::<uuid>` + Flat + JWT Claims)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GrokAuthCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_epoch: Option<i64>,
    pub user_id: Option<String>,
    pub email: Option<String>,
}

impl GrokAuthCredentials {
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at_epoch
            .is_some_and(|exp| now.timestamp() >= exp.saturating_sub(60))
    }

    pub fn dedup_identity(&self) -> Option<String> {
        if let Some(ref uid) = self.user_id {
            if !uid.is_empty() {
                return Some(uid.clone());
            }
        }
        if let Some(ref email) = self.email {
            if !email.is_empty() {
                return Some(email.to_ascii_lowercase());
            }
        }
        if !self.access_token.is_empty() {
            let suffix_len = self.access_token.len().min(24);
            return Some(format!(
                "tok:{}",
                &self.access_token[self.access_token.len() - suffix_len..]
            ));
        }
        None
    }
}

fn read_grok_auth_from_path(path: &Path) -> Option<GrokAuthCredentials> {
    let auth_file = if path.is_file() {
        path.to_path_buf()
    } else {
        path.join("auth.json")
    };
    let raw = fs::read_to_string(auth_file).ok()?;
    parse_grok_auth_json(&raw)
}

/// Parses Grok `auth.json` supporting both:
/// 1. Official nested `"https://auth.x.ai::<uuid>": { "key": "...", "auth_mode": "oauth", ... }`
/// 2. Flat `{"access_token": "...", "refresh_token": "...", "email": "..."}`
pub fn parse_grok_auth_json(raw: &str) -> Option<GrokAuthCredentials> {
    let val: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = val.as_object()?;

    // 1. Nested `https://auth.x.ai::<uuid>` entry (preferred) or any nested object containing `"key"`
    let nested_entry = obj
        .iter()
        .find(|(k, v)| k.starts_with("https://auth.x.ai::") && v.is_object())
        .or_else(|| {
            obj.iter()
                .find(|(_, v)| v.as_object().is_some_and(|o| o.contains_key("key")))
        });

    if let Some((_, nested_val)) = nested_entry {
        let inner = nested_val.as_object()?;
        let token = inner
            .get("key")
            .or_else(|| inner.get("access_token"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())?;

        let mut creds = parse_grok_token_claims(token);
        if let Some(rt) = inner
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            creds.refresh_token = Some(rt.to_string());
        }
        if let Some(exp_str) = inner.get("expires_at").and_then(|v| v.as_str()) {
            if let Ok(dt) = DateTime::parse_from_rfc3339(exp_str.trim()) {
                creds.expires_at_epoch = Some(dt.timestamp());
            }
        }
        if let Some(uid) = inner
            .get("user_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            creds.user_id = Some(uid.to_string());
        }
        if let Some(email) = inner
            .get("email")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            creds.email = Some(email.to_string());
        }
        return Some(creds);
    }

    // 2. Flat `{"access_token": "...", ...}` or `{"key": "..."}`
    let flat_tok = obj
        .get("access_token")
        .or_else(|| obj.get("key"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?;

    let mut creds = parse_grok_token_claims(flat_tok);
    if let Some(rt) = obj
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        creds.refresh_token = Some(rt.to_string());
    }
    if let Some(email) = obj
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        creds.email = Some(email.to_string());
    }
    if let Some(uid) = obj
        .get("user_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        creds.user_id = Some(uid.to_string());
    }

    Some(creds)
}

fn parse_grok_token_claims(token: &str) -> GrokAuthCredentials {
    let claims = decode_jwt_payload(token);
    let email = claims
        .as_ref()
        .and_then(|c| c.get("email"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let user_id = claims
        .as_ref()
        .and_then(|c| c.get("sub"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let exp = claims
        .as_ref()
        .and_then(|c| c.get("exp"))
        .and_then(|v| v.as_i64());

    GrokAuthCredentials {
        access_token: token.to_string(),
        refresh_token: None,
        expires_at_epoch: exp,
        user_id,
        email,
    }
}

fn build_grok_login_command(
    path: Option<&Path>,
    source_label: Option<&str>,
    is_system_default: bool,
) -> String {
    if is_system_default {
        return "grok login".to_string();
    }
    let dir_str = source_label
        .map(String::from)
        .or_else(|| path.map(|p| p.to_string_lossy().into_owned()));
    match dir_str {
        Some(d) if !d.is_empty() && d != "GROK_OAUTH_TOKEN" => {
            if d.contains(' ') {
                format!("GROK_HOME=\"{d}\" grok login")
            } else {
                format!("GROK_HOME={d} grok login")
            }
        }
        _ => "grok login".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tier 1: Billing Endpoint Parser (with Protobuf Omitted-Zero Fix)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GrokBillingEnvelope {
    #[serde(default)]
    config: Option<GrokBillingConfig>,
    #[serde(default)]
    result: Option<GrokStdioResult>,
}

#[derive(Debug, Deserialize)]
struct GrokStdioResult {
    #[serde(default)]
    config: Option<GrokBillingConfig>,
}

#[derive(Debug, Deserialize)]
struct GrokBillingConfig {
    #[serde(rename = "creditUsagePercent", default)]
    credit_usage_percent: Option<f64>,
    #[serde(rename = "currentPeriod", default)]
    current_period: Option<GrokPeriod>,
    #[serde(rename = "billingPeriodEnd", default)]
    billing_period_end: Option<String>,
    #[serde(rename = "onDemandCap", default)]
    on_demand_cap: Option<GrokValWrapper>,
    #[serde(rename = "onDemandUsed", default)]
    on_demand_used: Option<GrokValWrapper>,
    #[serde(rename = "prepaidBalance", default)]
    prepaid_balance: Option<GrokValWrapper>,
}

#[derive(Debug, Deserialize)]
struct GrokPeriod {
    #[serde(default)]
    end: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GrokValWrapper {
    #[serde(default)]
    val: Option<f64>,
}

/// Parses Grok `/v1/billing?format=credits` JSON, safely handling xAI's protobuf serialization
/// quirk where `creditUsagePercent` is omitted entirely at `0%` usage!
pub fn parse_grok_billing_json<T: TimeZone>(
    raw: &str,
    now: DateTime<Utc>,
    tz: &T,
) -> Result<(Vec<UsageBucket>, Option<CreditSummary>)>
where
    T::Offset: std::fmt::Display,
{
    let env: GrokBillingEnvelope =
        serde_json::from_str(raw).context("Failed to parse Grok billing JSON")?;

    let cfg = env
        .config
        .or_else(|| env.result.and_then(|r| r.config))
        .ok_or_else(|| anyhow!("Missing 'config' object in Grok billing response"))?;

    let period_end_raw = cfg
        .current_period
        .as_ref()
        .and_then(|p| p.end.as_deref())
        .or(cfg.billing_period_end.as_deref());
    let has_subscription_period = cfg.current_period.is_some() || cfg.billing_period_end.is_some();

    let on_demand_used = cfg.on_demand_used.and_then(|w| w.val).unwrap_or(0.0);
    let on_demand_cap = cfg.on_demand_cap.and_then(|w| w.val).unwrap_or(0.0);
    let prepaid_bal = cfg.prepaid_balance.and_then(|w| w.val).unwrap_or(0.0);

    // Protobuf default-zero omission rule:
    // If `creditUsagePercent` is None:
    // - If no subscription period applies and onDemandUsed > 0 & onDemandCap > 0, compute ratio.
    // - If subscription period is present (freshly reset weekly pool), default to `0.0`!
    let used_pct = match cfg.credit_usage_percent {
        Some(pct) => pct,
        None if !has_subscription_period && on_demand_used > 0.0 && on_demand_cap > 0.0 => {
            (on_demand_used / on_demand_cap) * 100.0
        }
        None if has_subscription_period => 0.0,
        None if on_demand_cap > 0.0 => (on_demand_used / on_demand_cap) * 100.0,
        None => return Err(anyhow!("Grok billing response contained no period or usage fields")),
    };

    let norm_reset = period_end_raw.and_then(|r| reset::normalize_reset(r, now, tz));
    let buckets = vec![UsageBucket::new(
        "wk",
        "Weekly",
        "wk",
        BucketLayout::InlinePair,
        true,
        used_pct,
        norm_reset,
    )];

    let credits = if on_demand_used > 0.0 && on_demand_cap > 0.0 {
        let pct = ((on_demand_used / on_demand_cap) * 100.0)
            .clamp(0.0, 100.0)
            .round() as u8;
        Some(CreditSummary {
            label: "On-demand".to_string(),
            used_dollars: Some(on_demand_used),
            limit_dollars: Some(on_demand_cap),
            balance_dollars: if prepaid_bal > 0.0 {
                Some(prepaid_bal)
            } else {
                None
            },
            used_percent: Some(pct),
        })
    } else if prepaid_bal > 0.0 {
        Some(CreditSummary {
            label: "Prepaid".to_string(),
            used_dollars: None,
            limit_dollars: None,
            balance_dollars: Some(prepaid_bal),
            used_percent: None,
        })
    } else {
        None
    };

    Ok((buckets, credits))
}

fn fetch_via_grok_stdio(
    grok_bin: &Path,
    profile_path: Option<&Path>,
    ctx: &FetchContext,
) -> Result<(Vec<UsageBucket>, Option<CreditSummary>)> {
    let req = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"_x.ai/billing\",\"params\":{}}\n";
    let mut spec = CommandSpec::new(grok_bin, ctx.timeout)
        .args(["agent", "stdio"])
        .stdin_bytes(req.as_bytes());

    if let Some(p) = profile_path {
        let dir = if p.is_file() {
            p.parent().unwrap_or(p)
        } else {
            p
        };
        spec = spec.env("GROK_HOME", dir.to_string_lossy().into_owned());
    }

    let line = run_jsonrpc_line_query(&spec, 2)?;
    parse_grok_billing_json(&line, ctx.now, &Local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_nested_auth_x_ai_uuid_credentials() {
        let raw = r#"{
          "https://auth.x.ai::8f4b2c10-9a1e-4d3a-b822-1c9e7f00a123": {
            "key": "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ1c3JfMTIzIn0.sig",
            "auth_mode": "oauth",
            "refresh_token": "def50200abc",
            "expires_at": "2026-11-17T18:00:00Z",
            "user_id": "usr_999",
            "email": "grok-user@example.com"
          }
        }"#;

        let creds = parse_grok_auth_json(raw).expect("parse nested grok auth.json");
        assert_eq!(
            creds.access_token,
            "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ1c3JfMTIzIn0.sig"
        );
        assert_eq!(creds.refresh_token.as_deref(), Some("def50200abc"));
        assert_eq!(creds.user_id.as_deref(), Some("usr_999"));
        assert_eq!(creds.email.as_deref(), Some("grok-user@example.com"));
        assert_eq!(creds.dedup_identity().as_deref(), Some("usr_999"));
    }

    #[test]
    fn test_parse_grok_billing_normal_and_protobuf_omitted_zero() {
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 18, 0, 0).unwrap();

        let normal_json = r#"{
          "config": {
            "creditUsagePercent": 10,
            "currentPeriod": {
              "type": "USAGE_PERIOD_TYPE_WEEKLY",
              "start": "2026-11-04T15:13:00.000000+00:00",
              "end": "2026-11-11T15:13:00.000000+00:00"
            },
            "onDemandCap": { "val": 100 },
            "onDemandUsed": { "val": 0 },
            "prepaidBalance": { "val": 25 }
          }
        }"#;
        let (buckets, credits) =
            parse_grok_billing_json(normal_json, now, &Utc).expect("parse normal billing");
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].id, "wk");
        assert_eq!(buckets[0].used_percent, 10);
        assert_eq!(buckets[0].remaining_percent, 90);
        assert_eq!(credits.unwrap().balance_dollars, Some(25.0));

        // Omitted `creditUsagePercent` right after weekly reset MUST succeed as 0%, NOT error!
        let zero_omitted_json = r#"{
          "config": {
            "currentPeriod": {
              "type": "USAGE_PERIOD_TYPE_WEEKLY",
              "start": "2026-11-10T15:13:00.000000+00:00",
              "end": "2026-11-17T15:13:00.000000+00:00"
            },
            "onDemandCap": { "val": 100 }
          }
        }"#;
        let (zero_buckets, _) = parse_grok_billing_json(zero_omitted_json, now, &Utc)
            .expect("omitted protobuf zero must parse as 0%");
        assert_eq!(zero_buckets[0].used_percent, 0);
        assert_eq!(zero_buckets[0].remaining_percent, 100);
    }
}
