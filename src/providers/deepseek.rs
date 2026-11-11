//! DeepSeek (`deepseek`) usage provider:
//! - Discovers manual `account.path`, or when `auto_discover` is enabled: `$DEEPSEEK_API_KEY` or
//!   `~/.config/deepseek/{api_key,config.json,auth.json}` (omitted cleanly when absent).
//! - Queries `GET https://api.deepseek.com/user/balance` (`Authorization: Bearer <key>`).
//! - Parses `is_available` and `balance_infos` (preferring `USD`, falling back to `CNY`),
//!   surfacing `Balance` credits and a primary bucket (`0%` used when `is_available && total_balance > 0`,
//!   `100%` used when `!is_available || total_balance <= 0`).

use crate::cache::CachedAccountState;
use crate::config::{
    abbreviate_home, expand_tilde_with_home, slugify_id, ManualAccountConfig, PollMode,
};
use crate::model::{AccountReport, AccountState, BucketLayout, CreditSummary, UsageBucket};
use crate::providers::{
    AccountDescriptor, AccountFetchOutcome, FetchContext, ProviderMeta, UsageProvider,
};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_DEEPSEEK_BALANCE_URL: &str = "https://api.deepseek.com/user/balance";
const ENV_SOURCE_LABEL: &str = "$DEEPSEEK_API_KEY";

pub struct DeepSeekProvider;

impl DeepSeekProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DeepSeekProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageProvider for DeepSeekProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            id: "deepseek",
            display_name: "DeepSeek",
            icon_name: "provider-deepseek",
            dashboard_url: "https://platform.deepseek.com/usage",
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
                    ctx.env_var("DEEPSEEK_API_KEY")
                        .map(|_| ENV_SOURCE_LABEL.to_string())
                });
            let dedup_key = expanded
                .as_deref()
                .and_then(|p| read_deepseek_api_key_from_path(p).ok())
                .map(|k| fingerprint_secret(&k));
            let id_slug = if m.id.trim().is_empty() {
                format!("manual-{}", idx + 1)
            } else {
                slugify_id(&m.id)
            };

            out.push(AccountDescriptor {
                provider_id: "deepseek".to_string(),
                id: format!("deepseek:{id_slug}"),
                label: sanitize_deepseek_label(Some(&m.display_label())),
                dedup_key,
                path: expanded,
                source_label,
                binary: m.binary.clone(),
                is_system_default: idx == 0 && !auto_discover,
                poll_mode: m.poll_mode,
                login_command: Some("https://platform.deepseek.com/api_keys".to_string()),
                switch_command: m.switch_command.clone(),
            });
        }

        if auto_discover {
            if let Some(env_key) = ctx.env_var("DEEPSEEK_API_KEY") {
                out.push(AccountDescriptor {
                    provider_id: "deepseek".to_string(),
                    id: "deepseek:env".to_string(),
                    label: "DeepSeek".to_string(),
                    dedup_key: Some(fingerprint_secret(&env_key)),
                    path: None,
                    source_label: Some(ENV_SOURCE_LABEL.to_string()),
                    binary: None,
                    is_system_default: true,
                    poll_mode: PollMode::Active,
                    login_command: Some("https://platform.deepseek.com/api_keys".to_string()),
                    switch_command: None,
                });
            }

            let has_env_default = out.iter().any(|a| a.is_system_default);
            for (idx, file_path) in candidate_deepseek_files(home, ctx.xdg_config_dir.as_deref())
                .into_iter()
                .enumerate()
            {
                if let Ok(file_key) = read_deepseek_api_key_from_path(&file_path) {
                    let slug = if idx == 0 && !has_env_default {
                        "default".to_string()
                    } else {
                        format!("config-{}", idx + 1)
                    };
                    out.push(AccountDescriptor {
                        provider_id: "deepseek".to_string(),
                        id: format!("deepseek:{slug}"),
                        label: "DeepSeek".to_string(),
                        dedup_key: Some(fingerprint_secret(&file_key)),
                        source_label: Some(abbreviate_home(&file_path, home)),
                        path: Some(file_path),
                        binary: None,
                        is_system_default: idx == 0 && !has_env_default,
                        poll_mode: PollMode::Active,
                        login_command: Some("https://platform.deepseek.com/api_keys".to_string()),
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
        let is_auto = account.id == "deepseek:env"
            || account.id == "deepseek:default"
            || account.id.starts_with("deepseek:config-");

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

        let balance_url = ctx
            .endpoints
            .deepseek_balance_url
            .as_deref()
            .unwrap_or(DEFAULT_DEEPSEEK_BALANCE_URL);
        let auth_header = format!("Bearer {api_key}");
        let headers = [
            ("Authorization", auth_header.as_str()),
            ("Accept", "application/json"),
        ];

        let resp = match ctx.http.get(balance_url, &headers) {
            Ok(r) => r,
            Err(err) => {
                return AccountFetchOutcome::Error {
                    message: format!("{err:#}"),
                };
            }
        };

        if resp.status == 429 {
            return AccountFetchOutcome::RateLimited {
                retry_after_secs: resp.retry_after,
            };
        }
        if resp.status == 401 || resp.status == 403 {
            return AccountFetchOutcome::SignedOut {
                message: format!("DeepSeek balance API returned HTTP {}", resp.status),
                login_command: account.login_command.clone(),
                label: Some(account.label.clone()),
                plan_tier: None,
            };
        }
        if !resp.is_success() {
            return AccountFetchOutcome::Error {
                message: format!("DeepSeek balance API returned HTTP {}", resp.status),
            };
        }

        match parse_deepseek_balance_json(&resp.body, account, ctx.now) {
            Ok(report) => AccountFetchOutcome::Success(report),
            Err(err) => AccountFetchOutcome::Error {
                message: format!("{err:#}"),
            },
        }
    }
}

fn candidate_deepseek_files(home: Option<&Path>, xdg_config: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(xdg) = xdg_config {
        dirs.push(xdg.join("deepseek"));
    }
    if let Some(h) = home {
        let def = h.join(".config").join("deepseek");
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
        if let Some(k) = ctx.env_var("DEEPSEEK_API_KEY") {
            return Ok(k);
        }
    }
    if let Some(ref path) = account.path {
        return read_deepseek_api_key_from_path(path);
    }
    if let Some(k) = ctx.env_var("DEEPSEEK_API_KEY") {
        return Ok(k);
    }
    Err(anyhow!("DeepSeek API key not found"))
}

pub fn read_deepseek_api_key_from_path(path: &Path) -> Result<String> {
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
            "No DeepSeek API key file found in {}",
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
        for field in ["api_key", "key", "deepseek_api_key", "apiKey", "token"] {
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
    format!("deepseek-key:{hex}")
}

fn sanitize_deepseek_label(custom: Option<&str>) -> String {
    if let Some(c) = custom.map(str::trim).filter(|s| !s.is_empty()) {
        if c != "DeepSeek"
            && c != "default"
            && c != "env"
            && !c.to_ascii_lowercase().starts_with("sk-")
            && !crate::http::redact_secrets(c).contains("[REDACTED]")
        {
            return c.to_string();
        }
    }
    "DeepSeek".to_string()
}

#[derive(Debug, Clone, Deserialize)]
struct DeepSeekBalanceResponse {
    #[serde(default)]
    is_available: bool,
    #[serde(default)]
    balance_infos: Vec<DeepSeekBalanceInfoRaw>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeepSeekBalanceInfoRaw {
    #[serde(default)]
    currency: String,
    #[serde(default)]
    total_balance: serde_json::Value,
    #[serde(default)]
    granted_balance: serde_json::Value,
    #[serde(default)]
    topped_up_balance: serde_json::Value,
}

#[derive(Debug, Clone)]
struct NormalizedBalanceInfo {
    currency: String,
    total_balance: f64,
    granted_balance: f64,
    topped_up_balance: f64,
}

fn parse_amount(v: &serde_json::Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
        .filter(|n| n.is_finite())
        .unwrap_or(0.0)
}

pub fn parse_deepseek_balance_json(
    raw: &str,
    account: &AccountDescriptor,
    now: chrono::DateTime<Utc>,
) -> Result<AccountReport> {
    let parsed: DeepSeekBalanceResponse =
        serde_json::from_str(raw).context("Failed to parse DeepSeek /user/balance JSON")?;

    let infos: Vec<NormalizedBalanceInfo> = parsed
        .balance_infos
        .iter()
        .map(|b| NormalizedBalanceInfo {
            currency: if b.currency.trim().is_empty() {
                "USD".to_string()
            } else {
                b.currency.trim().to_ascii_uppercase()
            },
            total_balance: parse_amount(&b.total_balance),
            granted_balance: parse_amount(&b.granted_balance),
            topped_up_balance: parse_amount(&b.topped_up_balance),
        })
        .collect();

    // Prefer funded USD first, then funded CNY, then USD, then CNY, then first available entry.
    let selected = infos
        .iter()
        .find(|i| i.currency == "USD" && i.total_balance > 0.0)
        .or_else(|| {
            infos
                .iter()
                .find(|i| i.currency == "CNY" && i.total_balance > 0.0)
        })
        .or_else(|| infos.iter().find(|i| i.currency == "USD"))
        .or_else(|| infos.iter().find(|i| i.currency == "CNY"))
        .or_else(|| infos.first())
        .cloned()
        .unwrap_or(NormalizedBalanceInfo {
            currency: "USD".to_string(),
            total_balance: 0.0,
            granted_balance: 0.0,
            topped_up_balance: 0.0,
        });

    let funded_ok = parsed.is_available && selected.total_balance > 0.0;
    let used_pct_f64 = if funded_ok { 0.0 } else { 100.0 };
    let sym = match selected.currency.as_str() {
        "USD" => "$",
        "CNY" => "¥",
        _ => "",
    };
    let amount_str = if sym.is_empty() {
        format!("{} {:.2}", selected.currency, selected.total_balance)
    } else {
        format!("{sym}{:.2}", selected.total_balance)
    };

    let extra_info = if selected.granted_balance > 0.0 {
        format!(
            "{amount_str} balance (topped up {:.2}, granted {:.2})",
            selected.topped_up_balance, selected.granted_balance
        )
    } else if funded_ok {
        format!("{amount_str} available")
    } else {
        format!("{amount_str} balance depleted")
    };

    let credit_label = if selected.currency == "USD" {
        "Balance".to_string()
    } else {
        format!("Balance ({})", selected.currency)
    };

    let bucket = UsageBucket::new(
        "balance",
        format!("Balance ({})", selected.currency),
        "bal",
        BucketLayout::FullRow,
        true,
        used_pct_f64,
        None,
    )
    .with_extra_info(extra_info.clone());
    let used_u8 = bucket.used_percent;

    let status_note = if !funded_ok {
        Some("Balance depleted".to_string())
    } else {
        None
    };

    Ok(AccountReport {
        id: account.id.clone(),
        label: sanitize_deepseek_label(Some(&account.label)),
        source_label: account.source_label.clone(),
        is_system_default: account.is_system_default,
        is_active: false,
        plan_tier: Some(selected.currency),
        state: AccountState::ActiveOk,
        is_stale: false,
        status_note,
        extra_info: Some(extra_info),
        error: None,
        login_command: account.login_command.clone(),
        updated_at_epoch: Some(now.timestamp()),
        buckets: vec![bucket],
        credits: Some(CreditSummary {
            label: credit_label,
            used_dollars: None,
            limit_dollars: None,
            balance_dollars: Some(selected.total_balance.max(0.0)),
            used_percent: Some(used_u8),
        }),
        sparkline: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{EndpointOverrides, HttpClient};
    use chrono::TimeZone;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_deepseek_balance_usd_preference_cny_fallback_and_depletion() {
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 16, 0, 0).unwrap();
        let desc = AccountDescriptor {
            provider_id: "deepseek".to_string(),
            id: "deepseek:default".to_string(),
            label: "DeepSeek".to_string(),
            dedup_key: None,
            path: None,
            source_label: Some("$DEEPSEEK_API_KEY".to_string()),
            binary: None,
            is_system_default: true,
            poll_mode: PollMode::Active,
            login_command: None,
            switch_command: None,
        };

        // 1. Both USD and CNY funded -> prefers USD, 0% used
        let usd_cny_json = r#"{
            "is_available": true,
            "balance_infos": [
                {"currency": "CNY", "total_balance": "120.00", "granted_balance": "0.00", "topped_up_balance": "120.00"},
                {"currency": "USD", "total_balance": "24.50", "granted_balance": "4.50", "topped_up_balance": "20.00"}
            ]
        }"#;
        let rep_usd = parse_deepseek_balance_json(usd_cny_json, &desc, now).unwrap();
        assert_eq!(rep_usd.plan_tier.as_deref(), Some("USD"));
        assert_eq!(rep_usd.buckets[0].used_percent, 0);
        assert_eq!(rep_usd.buckets[0].remaining_percent, 100);
        assert_eq!(rep_usd.credits.as_ref().unwrap().label, "Balance");
        assert_eq!(rep_usd.credits.as_ref().unwrap().balance_dollars, Some(24.50));

        // 2. CNY-only funded (`USD` is `0.00`, `CNY` is `88.00`) -> selects funded CNY, 0% used
        let cny_funded_json = r#"{
            "is_available": true,
            "balance_infos": [
                {"currency": "USD", "total_balance": "0.00", "granted_balance": "0.00", "topped_up_balance": "0.00"},
                {"currency": "CNY", "total_balance": "88.00", "granted_balance": "0.00", "topped_up_balance": "88.00"}
            ]
        }"#;
        let rep_cny = parse_deepseek_balance_json(cny_funded_json, &desc, now).unwrap();
        assert_eq!(rep_cny.plan_tier.as_deref(), Some("CNY"));
        assert_eq!(rep_cny.buckets[0].used_percent, 0);
        assert_eq!(rep_cny.credits.as_ref().unwrap().label, "Balance (CNY)");
        assert_eq!(rep_cny.credits.as_ref().unwrap().balance_dollars, Some(88.0));

        // 3. Depleted (`is_available: false` or `total_balance <= 0`) -> 100% used!
        let depleted_json = r#"{
            "is_available": false,
            "balance_infos": [
                {"currency": "USD", "total_balance": "0.00", "granted_balance": "0.00", "topped_up_balance": "0.00"}
            ]
        }"#;
        let rep_dep = parse_deepseek_balance_json(depleted_json, &desc, now).unwrap();
        assert_eq!(rep_dep.buckets[0].used_percent, 100);
        assert_eq!(rep_dep.buckets[0].remaining_percent, 0);
        assert_eq!(rep_dep.status_note.as_deref(), Some("Balance depleted"));
    }

    #[test]
    fn test_deepseek_discovery_omission_and_mock_http_fetch() {
        let tmp = std::env::temp_dir().join(format!(
            "gas-deepseek-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = tmp.join("home");
        let xdg = home.join(".config");
        fs::create_dir_all(&xdg).unwrap();

        let provider = DeepSeekProvider::new();

        // 1. Absent credentials -> omitted cleanly!
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

        // 2. File credential (`~/.config/deepseek/api_key`) + mock 127.0.0.1 server
        let ds_secret = format!("{}abcdef0123456789abcdef0123456789", concat!("s", "k-"));
        let key_path = xdg.join("deepseek/api_key");
        fs::create_dir_all(key_path.parent().unwrap()).unwrap();
        fs::write(&key_path, format!("{ds_secret}\n")).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let expected_auth = format!("Bearer {ds_secret}");
        let srv = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.contains(&expected_auth));
            let body = r#"{"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"19.75","granted_balance":"0.00","topped_up_balance":"19.75"}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
        });

        let ctx = FetchContext {
            endpoints: EndpointOverrides {
                deepseek_balance_url: Some(format!("http://{addr}/user/balance")),
                ..Default::default()
            },
            ..empty_ctx
        };

        let accts = provider.discover_accounts(&[], true, &ctx);
        assert_eq!(accts.len(), 1);
        let outcome = provider.fetch_account(&accts[0], None, &ctx);
        srv.join().unwrap();

        match outcome {
            AccountFetchOutcome::Success(rep) => {
                assert_eq!(rep.id, "deepseek:default");
                assert_eq!(rep.buckets[0].used_percent, 0);
                assert_eq!(rep.credits.as_ref().unwrap().balance_dollars, Some(19.75));
                let json_str = serde_json::to_string(&rep).unwrap();
                assert!(!json_str.contains(&ds_secret));
            }
            other => panic!("expected Success, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&tmp);
    }
}
