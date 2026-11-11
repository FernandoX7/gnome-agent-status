//! Gemini CLI provider (`POST https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota`).
//!
//! Strictly read-only credential discovery and quota collection:
//! - Auto-discovers ONLY when `~/.gemini/oauth_creds.json`, `~/.gemini-*/oauth_creds.json`,
//!   or `GEMINI_CLI_OAUTH_TOKEN` exists (NEVER triggers on `~/.gemini/antigravity` alone).
//! - Reads `oauth_creds.json` (`access_token`, `expiry_date`, `id_token` JWT email) and
//!   `google_accounts.json` (`active` email) with zero writes to `~/.gemini`.
//! - Queries `retrieveUserQuota` (and optional `loadCodeAssist` for plan tier) when `access_token`
//!   is valid/unexpired.
//! - Degrades gracefully to `AccountState::PassiveCached` (`"Updates when Gemini CLI runs"`)
//!   preserving cached buckets when `access_token` is expired, never rotating `refresh_token`
//!   behind Gemini CLI's back.

use crate::cache::CachedAccountState;
use crate::config::{abbreviate_home, expand_tilde, slugify_id, ManualAccountConfig, PollMode};
use crate::http::decode_jwt_payload;
use crate::model::{AccountReport, AccountState, BucketLayout, UsageBucket};
use crate::providers::{
    AccountDescriptor, AccountFetchOutcome, FetchContext, ProviderMeta, UsageProvider,
};
use crate::reset;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local, TimeZone, Utc};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_GEMINI_QUOTA_URL: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";
const DEFAULT_GEMINI_LOAD_URL: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const GEMINI_USER_AGENT: &str = "google-api-nodejs-client/9.15.1";
const DAILY_WINDOW_SECS: u64 = 86_400;

pub struct GeminiProvider;

impl Default for GeminiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiProvider {
    pub fn new() -> Self {
        Self
    }
}

impl UsageProvider for GeminiProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            id: "gemini",
            display_name: "Gemini CLI",
            icon_name: "provider-gemini",
            dashboard_url: "https://aistudio.google.com/",
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
            let creds_file = resolved_path.as_deref().and_then(resolve_gemini_creds_file);
            let meta = creds_file
                .as_deref()
                .and_then(inspect_gemini_identity_metadata);
            let source_label = resolved_path
                .as_deref()
                .map(|p| abbreviate_home(p, home_ref));
            let label = if m.label.is_some() {
                m.display_label()
            } else {
                meta.as_ref()
                    .and_then(|i| i.email.clone())
                    .unwrap_or_else(|| m.display_label())
            };

            out.push(AccountDescriptor {
                provider_id: "gemini".to_string(),
                id: format!("gemini:{}", slugify_id(&m.id)),
                label,
                dedup_key: meta.and_then(|i| i.dedup_key),
                path: resolved_path,
                source_label,
                binary: m.binary.clone(),
                is_system_default: idx == 0 && !auto_discover,
                poll_mode: m.poll_mode,
                login_command: Some("gemini".to_string()),
                switch_command: m.switch_command.clone(),
            });
        }

        if auto_discover {
            let mut found_default = false;

            if let Some(home) = home_ref {
                // Strictly check `~/.gemini/oauth_creds.json` — NEVER trigger on `~/.gemini/antigravity`!
                let default_creds = home.join(".gemini").join("oauth_creds.json");
                if default_creds.is_file() {
                    let meta = inspect_gemini_identity_metadata(&default_creds);
                    let email = meta.as_ref().and_then(|m| m.email.clone());
                    let id = email
                        .as_deref()
                        .map(|e| format!("gemini:{}", slugify_id(e)))
                        .unwrap_or_else(|| "gemini:default".to_string());
                    out.push(AccountDescriptor {
                        provider_id: "gemini".to_string(),
                        id,
                        label: email.unwrap_or_else(|| "Gemini CLI".to_string()),
                        dedup_key: meta.and_then(|m| m.dedup_key),
                        source_label: Some(abbreviate_home(&default_creds, Some(home))),
                        path: Some(default_creds),
                        binary: None,
                        is_system_default: true,
                        poll_mode: PollMode::Active,
                        login_command: Some("gemini".to_string()),
                        switch_command: None,
                    });
                    found_default = true;
                }

                // Scan `~/.gemini-*` and `~/.gemini_*` profile directories ONLY if they contain `oauth_creds.json`
                for dir in scan_gemini_profile_dirs(home) {
                    let creds = dir.join("oauth_creds.json");
                    if !creds.is_file() {
                        continue;
                    }
                    let suffix = dir
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("alt")
                        .trim_start_matches(".gemini-")
                        .trim_start_matches(".gemini_");
                    let meta = inspect_gemini_identity_metadata(&creds);
                    let email = meta.as_ref().and_then(|m| m.email.clone());
                    out.push(AccountDescriptor {
                        provider_id: "gemini".to_string(),
                        id: format!("gemini:{}", slugify_id(suffix)),
                        label: email.unwrap_or_else(|| format!("Gemini ({suffix})")),
                        dedup_key: meta.and_then(|m| m.dedup_key),
                        source_label: Some(abbreviate_home(&creds, Some(home))),
                        path: Some(creds),
                        binary: None,
                        is_system_default: false,
                        poll_mode: PollMode::Active,
                        login_command: Some("gemini".to_string()),
                        switch_command: None,
                    });
                }
            }

            if !found_default {
                if let Ok(env_tok) = env::var("GEMINI_CLI_OAUTH_TOKEN") {
                    let trimmed = env_tok.trim();
                    if !trimmed.is_empty() {
                        out.push(AccountDescriptor {
                            provider_id: "gemini".to_string(),
                            id: "gemini:default".to_string(),
                            label: "Gemini CLI".to_string(),
                            dedup_key: Some(fingerprint_gemini_token(trimmed)),
                            path: None,
                            source_label: Some("GEMINI_CLI_OAUTH_TOKEN".to_string()),
                            binary: None,
                            is_system_default: true,
                            poll_mode: PollMode::Active,
                            login_command: Some("gemini".to_string()),
                            switch_command: None,
                        });
                    }
                }
            }
        }

        out
    }

    fn fetch_account(
        &self,
        account: &AccountDescriptor,
        cached: Option<&CachedAccountState>,
        ctx: &FetchContext,
    ) -> AccountFetchOutcome {
        let now_epoch = ctx.now_epoch();
        let creds = match resolve_gemini_credentials(account, ctx) {
            Ok(c) => c,
            Err(err) => {
                return AccountFetchOutcome::SignedOut {
                    message: format!("{err:#}"),
                    login_command: account.login_command.clone(),
                    label: None,
                    plan_tier: cached.and_then(|c| c.plan_tier.clone()),
                };
            }
        };

        let resolved_label = resolve_gemini_label(account, creds.email.as_deref());

        // Never rotate `refresh_token` behind Gemini CLI's back! When `access_token` is missing
        // or expired, degrade gracefully to a passive cached snapshot.
        let is_expired = creds
            .expiry_epoch
            .is_some_and(|exp_epoch| exp_epoch <= now_epoch);
        let token_opt = creds
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        if is_expired || token_opt.is_none() {
            if creds.has_refresh_or_expired_creds || cached.is_some() {
                return build_passive_gemini_outcome(
                    account,
                    &resolved_label,
                    cached,
                    now_epoch,
                );
            }
            return AccountFetchOutcome::SignedOut {
                message: "Gemini CLI OAuth access token not found; run `gemini` to sign in"
                    .to_string(),
                login_command: account.login_command.clone(),
                label: Some(resolved_label),
                plan_tier: None,
            };
        }

        let access_token = token_opt.unwrap_or_default();
        let bearer = format!("Bearer {access_token}");
        let headers = [
            ("Authorization", bearer.as_str()),
            ("Accept", "application/json"),
            ("User-Agent", GEMINI_USER_AGENT),
        ];

        // Optional `loadCodeAssist` call for plan tier & companion project ID
        let assist_meta = query_load_code_assist(&headers, ctx);
        let plan_tier = assist_meta
            .as_ref()
            .and_then(|m| m.plan_tier.clone())
            .or_else(|| cached.and_then(|c| c.plan_tier.clone()));
        let quota_body = match assist_meta.as_ref().and_then(|m| m.project_id.as_deref()) {
            Some(proj) if !proj.is_empty() => serde_json::json!({ "project": proj }).to_string(),
            _ => "{}".to_string(),
        };

        let quota_url = ctx
            .endpoints
            .gemini_quota_url
            .as_deref()
            .unwrap_or(DEFAULT_GEMINI_QUOTA_URL);

        match ctx.http.post_json(quota_url, &headers, &quota_body) {
            Ok(resp) if resp.is_success() => {
                match parse_gemini_quota_json(&resp.body, ctx.now, &Local) {
                    Ok(buckets) => AccountFetchOutcome::Success(AccountReport {
                        id: account.id.clone(),
                        label: resolved_label,
                        source_label: account.source_label.clone(),
                        is_system_default: account.is_system_default,
                        is_active: false,
                        plan_tier,
                        state: AccountState::ActiveOk,
                        is_stale: false,
                        status_note: None,
                        extra_info: None,
                        error: None,
                        login_command: account.login_command.clone(),
                        updated_at_epoch: Some(now_epoch),
                        buckets,
                        credits: None,
                        sparkline: Vec::new(),
                    }),
                    Err(err) => AccountFetchOutcome::ErrorWithMeta {
                        message: format!("{err:#}"),
                        label: Some(resolved_label),
                        plan_tier,
                    },
                }
            }
            Ok(resp) if resp.status == 429 => AccountFetchOutcome::RateLimited {
                retry_after_secs: resp.retry_after,
            },
            // If token expired between disk read and request and a refresh_token exists on disk,
            // degrade to passive snapshot rather than showing a false hard auth error.
            Ok(resp) if resp.status == 401 && creds.has_refresh_or_expired_creds => {
                build_passive_gemini_outcome(account, &resolved_label, cached, now_epoch)
            }
            Ok(resp) if resp.status == 401 || resp.status == 403 => {
                AccountFetchOutcome::SignedOut {
                    message: format!("Gemini quota API returned HTTP {}", resp.status),
                    login_command: account.login_command.clone(),
                    label: Some(resolved_label),
                    plan_tier,
                }
            }
            Ok(resp) => AccountFetchOutcome::ErrorWithMeta {
                message: format!("Gemini quota API returned HTTP {}", resp.status),
                label: Some(resolved_label),
                plan_tier,
            },
            Err(err) => AccountFetchOutcome::ErrorWithMeta {
                message: format!("{err:#}"),
                label: Some(resolved_label),
                plan_tier,
            },
        }
    }
}

fn build_passive_gemini_outcome(
    account: &AccountDescriptor,
    resolved_label: &str,
    cached: Option<&CachedAccountState>,
    now_epoch: i64,
) -> AccountFetchOutcome {
    let mut buckets = cached.map(|c| c.buckets.clone()).unwrap_or_default();
    crate::cache::apply_bucket_time_decay(&mut buckets, now_epoch);

    AccountFetchOutcome::Passive(AccountReport {
        id: account.id.clone(),
        label: resolved_label.to_string(),
        source_label: account.source_label.clone(),
        is_system_default: account.is_system_default,
        is_active: false,
        plan_tier: cached.and_then(|c| c.plan_tier.clone()),
        state: AccountState::PassiveCached,
        is_stale: !buckets.is_empty(),
        status_note: Some("Updates when Gemini CLI runs".to_string()),
        extra_info: None,
        error: None,
        login_command: account.login_command.clone(),
        updated_at_epoch: cached.and_then(|c| c.last_success_epoch),
        buckets,
        credits: None,
        sparkline: cached.map(|c| c.sparkline.clone()).unwrap_or_default(),
    })
}

fn resolve_gemini_label(account: &AccountDescriptor, email: Option<&str>) -> String {
    let custom = account.label.trim();
    if !custom.is_empty() && custom != "Gemini CLI" && custom != "default" {
        return custom.to_string();
    }
    if let Some(em) = email.map(str::trim).filter(|s| !s.is_empty()) {
        return em.to_string();
    }
    if custom.is_empty() {
        "Gemini CLI".to_string()
    } else {
        custom.to_string()
    }
}

// ---------------------------------------------------------------------------
// Read-Only `~/.gemini/oauth_creds.json` & `google_accounts.json` Reader
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GeminiCredentials {
    pub access_token: Option<String>,
    pub expiry_epoch: Option<i64>,
    pub email: Option<String>,
    pub has_refresh_or_expired_creds: bool,
}

#[derive(Debug, Clone)]
struct GeminiIdentityMeta {
    email: Option<String>,
    dedup_key: Option<String>,
}

fn fingerprint_gemini_token(token: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(b"gemini:");
    hasher.update(token.trim().as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(24);
    hex.push_str("gem:");
    for b in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{b:02x}");
    }
    hex
}

fn scan_gemini_profile_dirs(home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(home) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let p = e.path();
            if !p.is_dir() {
                return None;
            }
            let name = p.file_name()?.to_str()?;
            if name.starts_with(".gemini-") || name.starts_with(".gemini_") {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    dirs.sort();
    dirs
}

fn resolve_gemini_creds_file(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if path.is_dir() {
        let candidate = path.join("oauth_creds.json");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn inspect_gemini_identity_metadata(creds_file: &Path) -> Option<GeminiIdentityMeta> {
    let creds = read_gemini_oauth_creds_file(creds_file).ok()?;
    let dedup_key = creds
        .email
        .as_deref()
        .map(|e| format!("email:{}", e.trim().to_ascii_lowercase()))
        .or_else(|| {
            creds
                .access_token
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(fingerprint_gemini_token)
        });
    Some(GeminiIdentityMeta {
        email: creds.email,
        dedup_key,
    })
}

fn resolve_gemini_credentials(
    account: &AccountDescriptor,
    ctx: &FetchContext,
) -> Result<GeminiCredentials> {
    if account.source_label.as_deref() == Some("GEMINI_CLI_OAUTH_TOKEN") {
        if let Ok(tok) = env::var("GEMINI_CLI_OAUTH_TOKEN") {
            let trimmed = tok.trim();
            if !trimmed.is_empty() {
                return Ok(GeminiCredentials {
                    access_token: Some(trimmed.to_string()),
                    expiry_epoch: None,
                    email: None,
                    has_refresh_or_expired_creds: false,
                });
            }
        }
    }

    if let Some(ref p) = account.path {
        let creds_file = resolve_gemini_creds_file(p).ok_or_else(|| {
            anyhow!(
                "Gemini CLI oauth_creds.json not found at {}",
                p.display()
            )
        })?;
        return read_gemini_oauth_creds_file(&creds_file);
    }

    if let Ok(tok) = env::var("GEMINI_CLI_OAUTH_TOKEN") {
        let trimmed = tok.trim();
        if !trimmed.is_empty() {
            return Ok(GeminiCredentials {
                access_token: Some(trimmed.to_string()),
                expiry_epoch: None,
                email: None,
                has_refresh_or_expired_creds: false,
            });
        }
    }

    if let Some(ref home) = ctx.home_dir {
        let default_creds = home.join(".gemini").join("oauth_creds.json");
        if default_creds.is_file() {
            return read_gemini_oauth_creds_file(&default_creds);
        }
    }

    Err(anyhow!(
        "Gemini CLI credentials not found in ~/.gemini/oauth_creds.json or GEMINI_CLI_OAUTH_TOKEN"
    ))
}

/// Reads `oauth_creds.json` and optional sibling `google_accounts.json` strictly read-only.
pub fn read_gemini_oauth_creds_file(creds_file: &Path) -> Result<GeminiCredentials> {
    let raw = fs::read_to_string(creds_file)
        .with_context(|| format!("Failed to read {}", creds_file.display()))?;
    let val: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse JSON in {}", creds_file.display()))?;

    let access_token = val
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);

    let expiry_epoch = val.get("expiry_date").and_then(parse_expiry_epoch);

    let has_refresh = val
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty());

    let jwt_email = val
        .get("id_token")
        .and_then(|v| v.as_str())
        .and_then(decode_jwt_payload)
        .and_then(|claims| {
            claims
                .get("email")
                .and_then(|e| e.as_str())
                .map(str::trim)
                .filter(|s| s.contains('@'))
                .map(ToString::to_string)
        });

    let accounts_email = creds_file
        .parent()
        .and_then(|dir| read_google_accounts_active_email(&dir.join("google_accounts.json")));

    Ok(GeminiCredentials {
        has_refresh_or_expired_creds: has_refresh || (access_token.is_some() && expiry_epoch.is_some()),
        access_token,
        expiry_epoch,
        email: jwt_email.or(accounts_email),
    })
}

fn parse_expiry_epoch(v: &serde_json::Value) -> Option<i64> {
    let raw_num = if let Some(i) = v.as_i64() {
        i
    } else if let Some(f) = v.as_f64() {
        if !f.is_finite() {
            return None;
        }
        f as i64
    } else {
        let s = v.as_str()?;
        let parsed = s.trim().parse::<f64>().ok()?;
        if !parsed.is_finite() {
            return None;
        }
        parsed as i64
    };

    if raw_num <= 0 {
        return None;
    }
    // `oauth_creds.json` stores `expiry_date` in Unix milliseconds (`>= 10^11`).
    if raw_num >= 100_000_000_000 {
        Some(raw_num / 1000)
    } else {
        Some(raw_num)
    }
}

pub fn read_google_accounts_active_email(accounts_path: &Path) -> Option<String> {
    let raw = fs::read_to_string(accounts_path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
    if let Some(active_str) = val.get("active").and_then(|v| v.as_str()) {
        let trimmed = active_str.trim();
        if trimmed.contains('@') {
            return Some(trimmed.to_string());
        }
    }
    if let Some(active_obj) = val.get("active").and_then(|v| v.as_object()) {
        if let Some(email) = active_obj
            .get("email")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| s.contains('@'))
        {
            return Some(email.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Cloud Code Private `loadCodeAssist` & `retrieveUserQuota` Parsers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GeminiAssistMeta {
    pub plan_tier: Option<String>,
    pub project_id: Option<String>,
}

fn query_load_code_assist(
    headers: &[(&str, &str)],
    ctx: &FetchContext,
) -> Option<GeminiAssistMeta> {
    // When running against a hermetic mock test server with `gemini_quota_url` overridden
    // and `gemini_load_url` omitted, skip hitting real `cloudcode-pa.googleapis.com`.
    let load_url = match (&ctx.endpoints.gemini_load_url, &ctx.endpoints.gemini_quota_url) {
        (Some(url), _) => url.as_str(),
        (None, Some(_)) => return None,
        (None, None) => DEFAULT_GEMINI_LOAD_URL,
    };

    let body = r#"{"metadata":{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}}"#;
    let resp = ctx.http.post_json(load_url, headers, body).ok()?;
    if !resp.is_success() {
        return None;
    }
    parse_load_code_assist_json(&resp.body)
}

pub fn parse_load_code_assist_json(raw: &str) -> Option<GeminiAssistMeta> {
    let val: serde_json::Value = serde_json::from_str(raw).ok()?;
    let project_id = match val.get("cloudaicompanionProject") {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(serde_json::Value::Object(obj)) => obj
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
        _ => None,
    };

    let raw_tier = val
        .get("paidTier")
        .and_then(|t| t.get("name").or_else(|| t.get("id")))
        .and_then(|v| v.as_str())
        .or_else(|| {
            val.get("currentTier")
                .and_then(|t| t.get("name").or_else(|| t.get("id")))
                .and_then(|v| v.as_str())
        });

    let plan_tier = raw_tier.and_then(normalize_gemini_tier_badge);
    Some(GeminiAssistMeta {
        plan_tier,
        project_id,
    })
}

pub fn normalize_gemini_tier_badge(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("enterprise") {
        Some("Enterprise".to_string())
    } else if lower.contains("ultra") {
        Some("Ultra".to_string())
    } else if lower.contains("standard") {
        Some("Standard".to_string())
    } else if lower.contains("pro") || lower.contains("ai-pro") {
        Some("Pro".to_string())
    } else if lower.contains("free") {
        Some("Free".to_string())
    } else {
        Some(trimmed.to_string())
    }
}

#[derive(Debug, Deserialize)]
struct RetrieveUserQuotaResponse {
    #[serde(default)]
    buckets: Vec<RawGeminiQuotaBucket>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawGeminiQuotaBucket {
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    remaining_fraction: Option<f64>,
    #[serde(default)]
    reset_time: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum GeminiBucketFamily {
    Pro,
    Flash,
    FlashLite,
    General,
}

impl GeminiBucketFamily {
    fn from_model_id(model_id: Option<&str>) -> Self {
        let lower = model_id.unwrap_or_default().trim().to_ascii_lowercase();
        if lower.contains("pro") || lower.contains("ultra") {
            Self::Pro
        } else if lower.contains("lite") {
            Self::FlashLite
        } else if lower.contains("flash") {
            Self::Flash
        } else {
            Self::General
        }
    }

    fn labels(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Pro => ("gemini-pro-daily", "Gemini Pro · Daily", "Pro 24h"),
            Self::Flash => ("gemini-flash-daily", "Gemini Flash · Daily", "Flash 24h"),
            Self::FlashLite => (
                "gemini-flash-lite-daily",
                "Gemini Flash Lite · Daily",
                "Lite 24h",
            ),
            Self::General => ("daily", "Gemini · Daily", "24h"),
        }
    }
}

/// Parses `v1internal:retrieveUserQuota` JSON into clean grouped daily `UsageBucket`s.
pub fn parse_gemini_quota_json<T: TimeZone>(
    raw: &str,
    now: DateTime<Utc>,
    tz: &T,
) -> Result<Vec<UsageBucket>>
where
    T::Offset: std::fmt::Display,
{
    let parsed: RetrieveUserQuotaResponse =
        serde_json::from_str(raw).context("Failed to parse Gemini retrieveUserQuota JSON")?;

    if parsed.buckets.is_empty() {
        return Err(anyhow!("Gemini retrieveUserQuota returned empty buckets[]"));
    }

    #[derive(Clone)]
    struct GroupAgg {
        min_remaining: f64,
        earliest_reset: Option<(i64, String)>,
    }

    let mut groups: std::collections::BTreeMap<GeminiBucketFamily, GroupAgg> =
        std::collections::BTreeMap::new();

    for b in &parsed.buckets {
        let _ = &b.token_type;
        let rem = match b.remaining_fraction {
            Some(f) if f.is_finite() => f.clamp(0.0, 1.0),
            // In protobuf-JSON, `0.0` may be omitted when quota is completely exhausted (`resetTime` present).
            None if b.reset_time.is_some() => 0.0,
            _ => continue,
        };
        let norm_reset = b
            .reset_time
            .as_deref()
            .and_then(|r| reset::normalize_reset(r, now, tz));
        let family = GeminiBucketFamily::from_model_id(b.model_id.as_deref());

        groups
            .entry(family)
            .and_modify(|agg| {
                if rem < agg.min_remaining {
                    agg.min_remaining = rem;
                }
                if let Some((ep, ref iso)) = norm_reset {
                    match agg.earliest_reset {
                        Some((cur_ep, _)) if ep >= cur_ep => {}
                        _ => agg.earliest_reset = Some((ep, iso.clone())),
                    }
                }
            })
            .or_insert(GroupAgg {
                min_remaining: rem,
                earliest_reset: norm_reset,
            });
    }

    if groups.is_empty() {
        return Err(anyhow!("No usable quota fractions in Gemini retrieveUserQuota"));
    }

    // Keep the card crisp: merge `FlashLite` into `Flash` when both share the exact same remaining %
    if let (Some(flash), Some(lite)) = (
        groups.get(&GeminiBucketFamily::Flash).cloned(),
        groups.get(&GeminiBucketFamily::FlashLite).cloned(),
    ) {
        let flash_pct = ((1.0 - flash.min_remaining) * 100.0).round() as u8;
        let lite_pct = ((1.0 - lite.min_remaining) * 100.0).round() as u8;
        if flash_pct == lite_pct {
            groups.remove(&GeminiBucketFamily::FlashLite);
        }
    }

    let mut out = Vec::with_capacity(groups.len());
    for (idx, (family, agg)) in groups.into_iter().enumerate() {
        let (id, label, short_label) = family.labels();
        out.push(
            UsageBucket::from_remaining_fraction(
                id,
                label,
                short_label,
                BucketLayout::FullRow,
                idx == 0,
                agg.min_remaining,
                agg.earliest_reset,
            )
            .with_window_seconds(Some(DAILY_WINDOW_SECS)),
        );
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use chrono::FixedOffset;

    #[test]
    fn test_never_autodiscovers_antigravity_without_oauth_creds_json() {
        let tmp = std::env::temp_dir().join(format!(
            "gas-gemini-no-agy-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = tmp.join("home");
        // Create `~/.gemini/antigravity` ONLY (matching hosts with Antigravity IDE but no Gemini CLI login)
        fs::create_dir_all(home.join(".gemini/antigravity")).unwrap();
        fs::write(home.join(".gemini/antigravity/state.vscdb"), b"sqlite").unwrap();

        let ctx = FetchContext {
            http: crate::http::HttpClient::new(std::time::Duration::from_secs(3)),
            timeout: std::time::Duration::from_secs(3),
            now: Utc.with_ymd_and_hms(2026, 11, 11, 16, 0, 0).unwrap(),
            home_dir: Some(home.clone()),
            xdg_config_dir: Some(home.join(".config")),
            proc_root: tmp.join("proc"),
            endpoints: Default::default(),
            env_overrides: Some(std::collections::BTreeMap::new()),
        };

        let provider = GeminiProvider::new();
        let discovered = provider.discover_accounts(&[], true, &ctx);
        assert!(
            discovered.is_empty(),
            "~/.gemini/antigravity alone MUST NEVER trigger Gemini CLI auto-discovery"
        );

        // Now add `~/.gemini/oauth_creds.json` + `google_accounts.json` -> discovered cleanly!
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"gemdev@example.com"}"#);
        let mock_id_jwt = format!("{}JhbGciOiJSUzI1NiJ9.{payload}.sig", concat!("ey", "J"));
        fs::write(
            home.join(".gemini/oauth_creds.json"),
            serde_json::json!({
                "access_token": "mock-gemini-access-token",
                "refresh_token": "mock-gemini-refresh-token",
                "expiry_date": 4102444800000i64,
                "id_token": mock_id_jwt
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            home.join(".gemini/google_accounts.json"),
            r#"{"active":"fallback@example.com","old":[]}"#,
        )
        .unwrap();

        let discovered2 = provider.discover_accounts(&[], true, &ctx);
        assert_eq!(discovered2.len(), 1);
        assert_eq!(discovered2[0].label, "gemdev@example.com");
        assert!(discovered2[0].is_system_default);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_parse_gemini_retrieve_user_quota_and_load_code_assist() {
        let tz = FixedOffset::east_opt(0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 16, 0, 0).unwrap();

        let quota_json = r#"{
            "buckets": [
                {
                    "modelId": "gemini-2.5-pro",
                    "tokenType": "REQUESTS",
                    "remainingFraction": 0.72,
                    "resetTime": "2026-11-12T00:00:00Z"
                },
                {
                    "modelId": "gemini-2.0-pro-exp",
                    "tokenType": "TOKENS",
                    "remainingFraction": 0.85,
                    "resetTime": "2026-11-12T00:00:00Z"
                },
                {
                    "modelId": "gemini-2.5-flash",
                    "tokenType": "REQUESTS",
                    "remainingFraction": 0.91,
                    "resetTime": "2026-11-12T00:00:00Z"
                },
                {
                    "modelId": "gemini-2.5-flash-lite",
                    "tokenType": "REQUESTS",
                    "remainingFraction": 0.91,
                    "resetTime": "2026-11-12T00:00:00Z"
                }
            ]
        }"#;

        let buckets = parse_gemini_quota_json(quota_json, now, &tz).expect("parse gemini quota");
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].id, "gemini-pro-daily");
        assert_eq!(buckets[0].label, "Gemini Pro · Daily");
        assert_eq!(buckets[0].short_label, "Pro 24h");
        assert!(buckets[0].is_primary);
        assert_eq!(buckets[0].used_percent, 28);
        assert_eq!(buckets[0].remaining_percent, 72);
        assert_eq!(buckets[0].window_seconds, Some(86_400));
        assert!(buckets[0].resets_at_epoch.is_some());

        assert_eq!(buckets[1].id, "gemini-flash-daily");
        assert_eq!(buckets[1].label, "Gemini Flash · Daily");
        assert!(!buckets[1].is_primary);
        assert_eq!(buckets[1].used_percent, 9);
        assert_eq!(buckets[1].window_seconds, Some(86_400));

        let assist_json = r#"{
            "cloudaicompanionProject": "my-gcp-proj-123",
            "currentTier": {
                "id": "standard-tier",
                "name": "Gemini Code Assist Standard"
            }
        }"#;
        let assist = parse_load_code_assist_json(assist_json).expect("parse loadCodeAssist");
        assert_eq!(assist.project_id.as_deref(), Some("my-gcp-proj-123"));
        assert_eq!(assist.plan_tier.as_deref(), Some("Standard"));
    }

    #[test]
    fn test_expired_gemini_token_degrades_gracefully_without_touching_creds_file() {
        let tmp = std::env::temp_dir().join(format!(
            "gas-gemini-expired-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let gemini_dir = tmp.join(".gemini");
        fs::create_dir_all(&gemini_dir).unwrap();
        let creds_path = gemini_dir.join("oauth_creds.json");
        let original_creds = r#"{"access_token":"expired-tok","refresh_token":"keep-untouched","expiry_date":1700000000000}"#;
        fs::write(&creds_path, original_creds).unwrap();
        fs::write(
            gemini_dir.join("google_accounts.json"),
            r#"{"active":"passive@example.com"}"#,
        )
        .unwrap();

        let ctx = FetchContext {
            http: crate::http::HttpClient::new(std::time::Duration::from_secs(3)),
            timeout: std::time::Duration::from_secs(3),
            now: Utc.with_ymd_and_hms(2026, 11, 11, 16, 0, 0).unwrap(),
            home_dir: Some(tmp.clone()),
            xdg_config_dir: Some(tmp.join(".config")),
            proc_root: tmp.join("proc"),
            endpoints: Default::default(),
            env_overrides: Some(std::collections::BTreeMap::new()),
        };

        let cached_entry = CachedAccountState {
            provider_id: "gemini".to_string(),
            account_id: "gemini:passive-example-com".to_string(),
            label: "passive@example.com".to_string(),
            source_label: Some("~/.gemini/oauth_creds.json".to_string()),
            is_system_default: true,
            plan_tier: Some("Standard".to_string()),
            state: AccountState::ActiveOk,
            status_note: None,
            extra_info: None,
            error: None,
            login_command: Some("gemini".to_string()),
            last_fetch_attempt_epoch: Some(1_794_400_000),
            last_success_epoch: Some(1_794_400_000),
            retry_after_epoch: None,
            consecutive_429s: 0,
            buckets: vec![UsageBucket::new(
                "gemini-pro-daily",
                "Gemini Pro · Daily",
                "Pro 24h",
                BucketLayout::FullRow,
                true,
                34.0,
                Some((1_794_480_000, "2026-11-12T10:40:00Z".to_string())),
            )
            .with_window_seconds(Some(86_400))],
            credits: None,
            sparkline: Vec::new(),
        };

        let provider = GeminiProvider::new();
        let discovered = provider.discover_accounts(&[], true, &ctx);
        assert_eq!(discovered.len(), 1);
        let outcome = provider.fetch_account(&discovered[0], Some(&cached_entry), &ctx);
        match outcome {
            AccountFetchOutcome::Passive(rep) => {
                assert_eq!(rep.state, AccountState::PassiveCached);
                assert_eq!(
                    rep.status_note.as_deref(),
                    Some("Updates when Gemini CLI runs")
                );
                assert_eq!(rep.plan_tier.as_deref(), Some("Standard"));
                assert_eq!(rep.buckets.len(), 1);
                assert_eq!(rep.buckets[0].used_percent, 34);
            }
            other => panic!("expected Passive outcome, got {other:?}"),
        }

        // Verify zero writes/mutations to `oauth_creds.json`!
        assert_eq!(fs::read_to_string(&creds_path).unwrap(), original_creds);

        let _ = fs::remove_dir_all(&tmp);
    }
}
