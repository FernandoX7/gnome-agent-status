//! Google Antigravity (`agy`) usage provider:
//! - Tier 1A: Pure-Rust `/proc` discovery + local `127.0.0.1` Connect-RPC `GetUserStatus` (~15ms, 60s TTL)
//! - Tier 1B: `agy -p /usage --output-format json` extracting all 4 model-family buckets (300s TTL)

use crate::cache::CachedAccountState;
use crate::config::{
    abbreviate_home, discover_provider_locations, expand_tilde_with_home, resolve_binary_for,
    ManualAccountConfig, PollMode,
};
use crate::model::{AccountReport, AccountState, BucketLayout, UsageBucket};
use crate::proc_exec::{run_command, CommandSpec};
use crate::providers::{
    AccountDescriptor, AccountFetchOutcome, FetchContext, ProviderMeta, UsageProvider,
};
use crate::reset;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local, TimeZone, Utc};
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

pub struct AntigravityProvider;

impl Default for AntigravityProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AntigravityProvider {
    pub fn new() -> Self {
        Self
    }
}

impl UsageProvider for AntigravityProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            id: "agy",
            display_name: "Antigravity",
            icon_name: "provider-agy",
            dashboard_url: "https://antigravity.google",
            default_ttl_secs: 300,
        }
    }

    fn effective_ttl_secs(&self, account: &AccountDescriptor, ctx: &FetchContext) -> u64 {
        if account.is_system_default && has_local_antigravity_server(ctx) {
            60
        } else {
            300
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
            // Only the first manual active entry (or explicitly "default") maps to the active keyring slot;
            // secondary manual entries without an active session must never mutate GNOME Keyring.
            let is_default = idx == 0 && m.poll_mode == PollMode::Active;

            out.push(AccountDescriptor {
                provider_id: "agy".to_string(),
                id: format!("agy:{}", m.id),
                label: m.display_label(),
                dedup_key: if is_default {
                    Some("agy-system-default".to_string())
                } else {
                    None
                },
                path: expanded,
                source_label,
                binary: m.binary.clone(),
                is_system_default: is_default,
                poll_mode: m.poll_mode,
                login_command: Some("agy auth login".to_string()),
                switch_command: m.switch_command.clone(),
            });
        }

        if auto_discover {
            let has_local_rpc = has_local_antigravity_server(ctx);
            let fs_locs =
                discover_provider_locations("agy", home, ctx.xdg_config_dir.as_deref());

            if fs_locs.is_empty() && has_local_rpc {
                out.push(AccountDescriptor {
                    provider_id: "agy".to_string(),
                    id: "agy:default".to_string(),
                    label: "Antigravity".to_string(),
                    dedup_key: Some("agy-system-default".to_string()),
                    path: None,
                    source_label: Some("local language_server".to_string()),
                    binary: None,
                    is_system_default: true,
                    poll_mode: PollMode::Active,
                    login_command: Some("agy auth login".to_string()),
                    switch_command: None,
                });
            }

            for loc in fs_locs {
                let meta_email = read_switcher_slot_email(&loc.path);
                let label = meta_email
                    .clone()
                    .unwrap_or_else(|| {
                        if loc.is_system_default {
                            "Antigravity".to_string()
                        } else {
                            loc.suggested_id.clone()
                        }
                    });
                let id = if let Some(ref email) = meta_email {
                    format!("agy:{}", email.to_ascii_lowercase())
                } else {
                    format!("agy:{}", loc.suggested_id)
                };

                out.push(AccountDescriptor {
                    provider_id: "agy".to_string(),
                    id,
                    label,
                    dedup_key: if loc.is_system_default {
                        Some("agy-system-default".to_string())
                    } else {
                        meta_email.map(|e| e.to_ascii_lowercase())
                    },
                    path: Some(loc.path),
                    source_label: Some(loc.source_label),
                    binary: None,
                    is_system_default: loc.is_system_default,
                    poll_mode: loc.poll_mode,
                    login_command: Some("agy auth login".to_string()),
                    switch_command: None,
                });
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

        // 1. Try local Connect-RPC `GetUserStatus` ONLY for the system-default slot (~10ms, zero process spawn).
        //    Secondary manual accounts must never be overwritten with the primary running IDE's session!
        let rpc_result = if account.is_system_default {
            query_local_rpc_status(ctx)
        } else {
            None
        };

        // 2. Determine whether we also want 4-bucket `agy -p /usage --output-format json`:
        //    - If local RPC succeeded AND cached already has a 4-bucket snapshot < 300s old,
        //      overlay live RPC quota onto cached 4-bucket snapshot for <15ms zero-spawn updates!
        //    - If local RPC succeeded AND `agy` CLI is not installed (and no custom binary configured),
        //      skip spawning nonexistent `agy` and use RPC buckets directly.
        //    - Otherwise try `agy -p /usage --output-format json` for explicit 5h + weekly split.
        let cached_4buckets_fresh = cached.is_some_and(|c| {
            c.buckets.len() >= 4
                && c.last_success_epoch
                    .is_some_and(|ts| (0..300).contains(&(now_epoch - ts)))
        });
        let has_agy_cli = account.binary.is_some() || resolve_binary_for("agy", None).is_file();

        let cli_result = if rpc_result.is_some() && (cached_4buckets_fresh || !has_agy_cli) {
            None
        } else {
            Some(fetch_via_agy_cli(account, ctx))
        };

        match (rpc_result, cli_result) {
            (Some(rpc_info), Some(Ok(cli_buckets))) => {
                let resolved_label = rpc_info
                    .email
                    .clone()
                    .unwrap_or_else(|| account.label.clone());
                let resolved_id = resolve_agy_account_id(account, rpc_info.email.as_deref());
                AccountFetchOutcome::Success(AccountReport {
                    id: resolved_id,
                    label: resolved_label,
                    source_label: account.source_label.clone(),
                    is_system_default: account.is_system_default,
                    is_active: false,
                    plan_tier: rpc_info.plan_tier,
                    state: AccountState::ActiveOk,
                    is_stale: false,
                    status_note: None,
                    error: None,
                    login_command: account.login_command.clone(),
                    updated_at_epoch: Some(now_epoch),
                    buckets: cli_buckets,
                    credits: None,
                })
            }
            (Some(rpc_info), None) => {
                let buckets = if cached_4buckets_fresh {
                    // Fast path: overlay live RPC values onto fresh cached 4-bucket windows
                    let mut b = cached
                        .map(|c| c.buckets.clone())
                        .unwrap_or_else(|| rpc_info.fallback_buckets.clone());
                    overlay_rpc_on_buckets(&mut b, &rpc_info);
                    b
                } else {
                    rpc_info.fallback_buckets.clone()
                };
                let resolved_label = rpc_info
                    .email
                    .clone()
                    .unwrap_or_else(|| account.label.clone());
                let resolved_id = resolve_agy_account_id(account, rpc_info.email.as_deref());
                AccountFetchOutcome::Success(AccountReport {
                    id: resolved_id,
                    label: resolved_label,
                    source_label: account.source_label.clone(),
                    is_system_default: account.is_system_default,
                    is_active: false,
                    plan_tier: rpc_info
                        .plan_tier
                        .or_else(|| cached.and_then(|c| c.plan_tier.clone())),
                    state: AccountState::ActiveOk,
                    is_stale: false,
                    status_note: None,
                    error: None,
                    login_command: account.login_command.clone(),
                    updated_at_epoch: Some(now_epoch),
                    buckets,
                    credits: None,
                })
            }
            (Some(rpc_info), Some(Err(_))) => {
                // Local IDE language_server is running even if `agy` CLI is absent or failed!
                let resolved_label = rpc_info
                    .email
                    .clone()
                    .unwrap_or_else(|| account.label.clone());
                let resolved_id = resolve_agy_account_id(account, rpc_info.email.as_deref());
                AccountFetchOutcome::Success(AccountReport {
                    id: resolved_id,
                    label: resolved_label,
                    source_label: account.source_label.clone(),
                    is_system_default: account.is_system_default,
                    is_active: false,
                    plan_tier: rpc_info.plan_tier,
                    state: AccountState::ActiveOk,
                    is_stale: false,
                    status_note: None,
                    error: None,
                    login_command: account.login_command.clone(),
                    updated_at_epoch: Some(now_epoch),
                    buckets: rpc_info.fallback_buckets,
                    credits: None,
                })
            }
            (None, Some(Ok(cli_buckets))) => AccountFetchOutcome::Success(AccountReport {
                id: account.id.clone(),
                label: account.label.clone(),
                source_label: account.source_label.clone(),
                is_system_default: account.is_system_default,
                is_active: false,
                plan_tier: cached.and_then(|c| c.plan_tier.clone()),
                state: AccountState::ActiveOk,
                is_stale: false,
                status_note: None,
                error: None,
                login_command: account.login_command.clone(),
                updated_at_epoch: Some(now_epoch),
                buckets: cli_buckets,
                credits: None,
            }),
            (None, Some(Err(err))) => {
                let msg = format!("{:#}", err);
                let lower = msg.to_ascii_lowercase();
                if lower.contains("sign in")
                    || lower.contains("not logged in")
                    || lower.contains("unauthenticated")
                    || lower.contains("auth login")
                {
                    AccountFetchOutcome::SignedOut {
                        message: msg,
                        login_command: account.login_command.clone(),
                        label: None,
                        plan_tier: None,
                    }
                } else {
                    AccountFetchOutcome::Error { message: msg }
                }
            }
            (None, None) => AccountFetchOutcome::Error {
                message: "No Antigravity transport available".to_string(),
            },
        }
    }
}

fn resolve_agy_account_id(account: &AccountDescriptor, email: Option<&str>) -> String {
    if account.id == "agy:default" {
        if let Some(e) = email.map(str::trim).filter(|s| !s.is_empty()) {
            return format!("agy:{}", e.to_ascii_lowercase());
        }
    }
    account.id.clone()
}

fn read_switcher_slot_email(path: &Path) -> Option<String> {
    let candidates = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        vec![
            path.join("metadata.json"),
            path.join("account.json"),
            path.join("profile.json"),
        ]
    };
    for file in candidates {
        if let Ok(raw) = fs::read_to_string(&file) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(email) = val
                    .get("email")
                    .or_else(|| val.get("account"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| s.contains('@'))
                {
                    return Some(email.to_string());
                }
            }
        }
    }
    None
}

fn current_euid() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(unsafe { libc::geteuid() })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Resolves the `ANTIGRAVITY_CLI_APP_DATA_DIR` override ONLY when `path` explicitly targets
/// an `antigravity-cli` profile directory (avoiding the v0.1 bug that pointed it at `~/.gemini`).
pub fn resolve_agy_cli_app_data_dir(path: &Path) -> Option<PathBuf> {
    if path.join("antigravity-cli").is_dir() {
        return Some(path.join("antigravity-cli"));
    }
    if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "antigravity-cli" || n.starts_with("antigravity-cli-"))
    {
        return Some(path.to_path_buf());
    }
    None
}

fn fetch_via_agy_cli(account: &AccountDescriptor, ctx: &FetchContext) -> Result<Vec<UsageBucket>> {
    let bin = resolve_binary_for("agy", account.binary.as_deref());
    let mut spec =
        CommandSpec::new(bin, ctx.timeout).args(["-p", "/usage", "--output-format", "json"]);

    if let Some(ref p) = account.path {
        if let Some(app_data) = resolve_agy_cli_app_data_dir(p) {
            spec = spec.env(
                "ANTIGRAVITY_CLI_APP_DATA_DIR",
                app_data.to_string_lossy().into_owned(),
            );
        }
    }

    let output = run_command(&spec)?;
    if !output.stdout.trim().is_empty() {
        if let Ok(buckets) = parse_agy_4buckets(&output.stdout, ctx.now, &Local) {
            return Ok(buckets);
        }
    }
    let combined = output.combined_text();
    if !output.status.success() {
        return Err(anyhow!(
            "agy exited with {}: {}",
            output.status,
            combined.lines().next().unwrap_or("unknown error")
        ));
    }
    parse_agy_4buckets(&combined, ctx.now, &Local)
}

// ---------------------------------------------------------------------------
// 4-Bucket `agy -p /usage --output-format json` Parser
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AgyCliPayload {
    #[serde(default)]
    response: Option<String>,
    #[serde(default)]
    command: Option<AgyCliCommand>,
}

#[derive(Debug, Deserialize)]
struct AgyCliCommand {
    #[serde(default)]
    data: Option<AgyCliData>,
}

#[derive(Debug, Deserialize)]
struct AgyCliData {
    #[serde(default)]
    groups: Vec<AgyCliGroup>,
}

#[derive(Debug, Deserialize)]
struct AgyCliGroup {
    #[serde(default)]
    name: String,
    #[serde(default)]
    buckets: Vec<AgyCliBucket>,
}

#[derive(Debug, Deserialize)]
struct AgyCliBucket {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    window: Option<String>,
    #[serde(default, alias = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(default, alias = "resetTime")]
    reset_time: Option<String>,
}

struct CanonicalAgySpec {
    id: &'static str,
    label: &'static str,
    short_label: &'static str,
    is_primary: bool,
    family_is_gemini: bool,
    window_is_weekly: bool,
}

const AGY_BUCKET_SPECS: [CanonicalAgySpec; 4] = [
    CanonicalAgySpec {
        id: "gemini-weekly",
        label: "Gemini · Weekly",
        short_label: "Gemini wk",
        is_primary: true,
        family_is_gemini: true,
        window_is_weekly: true,
    },
    CanonicalAgySpec {
        id: "gemini-5h",
        label: "Gemini · 5h",
        short_label: "Gemini 5h",
        is_primary: false,
        family_is_gemini: true,
        window_is_weekly: false,
    },
    CanonicalAgySpec {
        id: "3p-weekly",
        label: "Claude & GPT · Weekly",
        short_label: "Claude/GPT wk",
        is_primary: false,
        family_is_gemini: false,
        window_is_weekly: true,
    },
    CanonicalAgySpec {
        id: "3p-5h",
        label: "Claude & GPT · 5h",
        short_label: "Claude/GPT 5h",
        is_primary: false,
        family_is_gemini: false,
        window_is_weekly: false,
    },
];

/// Parses all 4 quota buckets (`Gemini · Weekly`, `Gemini · 5h`, `Claude & GPT · Weekly`,
/// `Claude & GPT · 5h`) from `agy -p /usage --output-format json`.
pub fn parse_agy_4buckets<T: TimeZone>(
    raw: &str,
    now: DateTime<Utc>,
    tz: &T,
) -> Result<Vec<UsageBucket>>
where
    T::Offset: std::fmt::Display,
{
    let trimmed = raw.trim();
    let json_slice = match (trimmed.find('{'), trimmed.rfind('}')) {
        (Some(start), Some(end)) if start < end => &trimmed[start..=end],
        _ => trimmed,
    };

    let parsed: AgyCliPayload =
        serde_json::from_str(json_slice).context("Failed to parse JSON output from agy")?;

    let mut out = Vec::new();

    if let Some(data) = parsed.command.and_then(|c| c.data) {
        for spec in &AGY_BUCKET_SPECS {
            let matched = data
                .groups
                .iter()
                .flat_map(|g| g.buckets.iter().map(move |b| (&g.name, b)))
                .find(|(_, b)| b.id == spec.id)
                .or_else(|| {
                    data.groups
                        .iter()
                        .filter(|g| {
                            let lower = g.name.to_ascii_lowercase();
                            if spec.family_is_gemini {
                                lower.contains("gemini")
                            } else {
                                lower.contains("claude") || lower.contains("gpt") || lower.contains("3p")
                            }
                        })
                        .flat_map(|g| g.buckets.iter().map(move |b| (&g.name, b)))
                        .find(|(_, b)| {
                            let w_lower = b.window.as_deref().unwrap_or_default().to_ascii_lowercase();
                            let n_lower = b.name.to_ascii_lowercase();
                            if spec.window_is_weekly {
                                w_lower == "weekly" || n_lower.contains("weekly")
                            } else {
                                w_lower == "5h" || n_lower.contains("five hour") || n_lower.contains("5h")
                            }
                        })
                });

            if let Some((_, b)) = matched {
                if let Some(rem_frac) = b.remaining_fraction {
                    let norm_reset = b
                        .reset_time
                        .as_deref()
                        .and_then(|r| reset::normalize_reset(r, now, tz));
                    out.push(UsageBucket::from_remaining_fraction(
                        spec.id,
                        spec.label,
                        spec.short_label,
                        BucketLayout::FullRow,
                        spec.is_primary,
                        rem_frac,
                        norm_reset,
                    ));
                }
            }
        }
    }

    // Fallback: parse TSV lines in `response` if `command.data.groups` was absent
    if out.is_empty() {
        if let Some(resp) = parsed.response {
            for spec in &AGY_BUCKET_SPECS {
                for line in resp.lines() {
                    let parts: Vec<&str> = line.split('\t').collect();
                    if parts.len() < 3 {
                        continue;
                    }
                    let grp = parts[0].to_ascii_lowercase();
                    let win = parts[1].to_ascii_lowercase();
                    let grp_match = if spec.family_is_gemini {
                        grp.contains("gemini")
                    } else {
                        grp.contains("claude") || grp.contains("gpt")
                    };
                    let win_match = if spec.window_is_weekly {
                        win.contains("weekly")
                    } else {
                        win.contains("five hour") || win.contains("5h")
                    };

                    if grp_match && win_match {
                        let pct_str = parts[2].trim().trim_end_matches('%');
                        if let Ok(rem_pct) = pct_str.parse::<f64>() {
                            let rem_frac = (rem_pct / 100.0).clamp(0.0, 1.0);
                            let norm_reset = parts
                                .get(3)
                                .map(|s| s.trim())
                                .and_then(|r| reset::normalize_reset(r, now, tz));
                            out.push(UsageBucket::from_remaining_fraction(
                                spec.id,
                                spec.label,
                                spec.short_label,
                                BucketLayout::FullRow,
                                spec.is_primary,
                                rem_frac,
                                norm_reset,
                            ));
                            break;
                        }
                    }
                }
            }
        }
    }

    if out.is_empty() {
        return Err(anyhow!("Could not find usage buckets in agy output"));
    }

    // Ensure at least one bucket is marked primary even if `gemini-weekly` was missing
    if !out.iter().any(|b| b.is_primary) {
        if let Some(first) = out.first_mut() {
            first.is_primary = true;
        }
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Pure-Rust `/proc` Discovery & Local Connect-RPC (`GetUserStatus`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcLanguageServerArgs {
    pub csrf_token: String,
    pub extension_server_port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalLanguageServer {
    pub pid: u32,
    pub csrf_token: String,
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonHubEndpoint {
    pub pid: u32,
    pub csrf_token: String,
    pub http_port: Option<u16>,
    pub https_port: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct DaemonHubFile {
    #[serde(rename = "csrfToken", default)]
    csrf_token: String,
    #[serde(rename = "httpPort", default)]
    http_port: Option<u16>,
    #[serde(rename = "httpsPort", default)]
    https_port: Option<u16>,
    #[serde(default)]
    pid: u32,
}

fn has_local_antigravity_server(ctx: &FetchContext) -> bool {
    let euid = current_euid();
    ctx.endpoints.agy_rpc_url.is_some()
        || !discover_daemon_hub_servers(ctx.home_dir.as_deref(), &ctx.proc_root, euid).is_empty()
        || !discover_local_servers_in(&ctx.proc_root, euid).is_empty()
}

/// Discovers Antigravity 2.x daemon hub files (`~/.gemini/antigravity/daemon/hub_<port>.json`),
/// verifying that each advertised `pid` is alive in `proc_root`, owned by `target_uid`, and
/// running `language_server` so stale hub files or recycled PIDs are safely ignored.
pub fn discover_daemon_hub_servers(
    home_dir: Option<&Path>,
    proc_root: &Path,
    target_uid: Option<u32>,
) -> Vec<DaemonHubEndpoint> {
    let Some(home) = home_dir else {
        return Vec::new();
    };
    if !proc_root.is_dir() {
        return Vec::new();
    }
    let daemon_dir = home.join(".gemini/antigravity/daemon");
    let Ok(entries) = fs::read_dir(&daemon_dir) else {
        return Vec::new();
    };

    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let p = entry.path();
        let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with("hub_") && name.ends_with(".json") && p.is_file() {
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            candidates.push((mtime, p));
        }
    }
    // Check newest hub files first
    candidates.sort_by_key(|a| std::cmp::Reverse(a.0));

    let mut out = Vec::new();
    for (_, hub_path) in candidates {
        let Ok(raw) = fs::read_to_string(&hub_path) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<DaemonHubFile>(&raw) else {
            continue;
        };
        let csrf_token = parsed.csrf_token.trim().to_string();
        if parsed.pid == 0
            || csrf_token.is_empty()
            || (parsed.http_port.is_none() && parsed.https_port.is_none())
        {
            continue;
        }

        if !verify_pid_is_language_server(proc_root, parsed.pid, target_uid) {
            continue;
        }

        out.push(DaemonHubEndpoint {
            pid: parsed.pid,
            csrf_token,
            http_port: parsed.http_port.filter(|&p| p > 0),
            https_port: parsed.https_port.filter(|&p| p > 0),
        });
    }
    out
}

fn verify_pid_is_language_server(
    proc_root: &Path,
    pid: u32,
    target_uid: Option<u32>,
) -> bool {
    let proc_dir = proc_root.join(pid.to_string());
    if !proc_dir.is_dir() {
        return false;
    }
    if let Some(expected_uid) = target_uid {
        #[cfg(unix)]
        {
            let Ok(meta) = fs::metadata(&proc_dir) else {
                return false;
            };
            if meta.uid() != expected_uid {
                return false;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = expected_uid;
        }
    }
    if let Ok(cmdline) = fs::read(proc_dir.join("cmdline")) {
        let lower = String::from_utf8_lossy(&cmdline).to_ascii_lowercase();
        if lower.contains("language_server") {
            return true;
        }
    }
    if let Ok(comm) = fs::read_to_string(proc_dir.join("comm")) {
        if comm.to_ascii_lowercase().contains("language_server") {
            return true;
        }
    }
    false
}

/// Pure-Rust parser for `/proc/<pid>/cmdline` (NUL-separated bytes).
/// Unit-tested on all operating systems.
pub fn parse_proc_cmdline(raw: &[u8]) -> Option<ProcLanguageServerArgs> {
    let args: Vec<String> = raw
        .split(|&b| b == 0)
        .filter(|slice| !slice.is_empty())
        .map(|slice| String::from_utf8_lossy(slice).into_owned())
        .collect();

    if args.is_empty() {
        return None;
    }

    let exe_lower = args[0].to_ascii_lowercase();
    if !exe_lower.contains("language_server") {
        return None;
    }

    let has_hub_subclient = args.windows(2).any(|w| {
        (w[0] == "--subclient_type" || w[0] == "--subclient-type")
            && w[1].eq_ignore_ascii_case("hub")
    }) || args.iter().any(|a| {
        let l = a.to_ascii_lowercase();
        l == "--subclient_type=hub" || l == "--subclient-type=hub"
    });

    let is_agy_server = exe_lower.contains("language_server_linux")
        || exe_lower.contains("antigravity")
        || has_hub_subclient
        || args.iter().any(|a| {
            let l = a.to_ascii_lowercase();
            l.contains("antigravity")
        });

    if !is_agy_server {
        return None;
    }

    let mut csrf_token: Option<String> = None;
    let mut extension_server_port: Option<u16> = None;

    let mut idx = 1;
    while idx < args.len() {
        let arg = args[idx].as_str();
        if arg == "--csrf_token" || arg == "--csrf-token" {
            if let Some(val) = args.get(idx + 1) {
                if !val.is_empty() {
                    csrf_token = Some(val.clone());
                }
                idx += 2;
                continue;
            }
        } else if let Some(val) = arg
            .strip_prefix("--csrf_token=")
            .or_else(|| arg.strip_prefix("--csrf-token="))
        {
            if !val.is_empty() {
                csrf_token = Some(val.to_string());
            }
        } else if arg == "--extension_server_port" || arg == "--extension-server-port" {
            if let Some(val) = args.get(idx + 1) {
                extension_server_port = val.parse::<u16>().ok();
                idx += 2;
                continue;
            }
        } else if let Some(val) = arg
            .strip_prefix("--extension_server_port=")
            .or_else(|| arg.strip_prefix("--extension-server-port="))
        {
            extension_server_port = val.parse::<u16>().ok();
        }
        idx += 1;
    }

    Some(ProcLanguageServerArgs {
        csrf_token: csrf_token?,
        extension_server_port,
    })
}

/// Extracts `<inode>` from a `/proc/<pid>/fd/*` symlink target like `"socket:[123456]"`.
pub fn parse_socket_inode_link(target: &str) -> Option<u64> {
    let trimmed = target.trim();
    let inner = trimmed.strip_prefix("socket:[")?.strip_suffix(']')?;
    inner.parse::<u64>().ok()
}

/// Pure-Rust parser for `/proc/net/tcp` and `/proc/net/tcp6`, matching listening (`st == 0A`)
/// loopback sockets whose `inode` belongs to `target_inodes`.
pub fn parse_proc_net_tcp(content: &str, target_inodes: &HashSet<u64>) -> Vec<u16> {
    if target_inodes.is_empty() {
        return Vec::new();
    }
    let mut ports = Vec::new();
    for line in content.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 10 {
            continue;
        }
        let local_addr = cols[1];
        let state = cols[3];
        if !state.eq_ignore_ascii_case("0A") {
            continue;
        }
        let Ok(inode) = cols[9].parse::<u64>() else {
            continue;
        };
        if !target_inodes.contains(&inode) {
            continue;
        }

        let Some((hex_ip, hex_port)) = local_addr.split_once(':') else {
            continue;
        };
        if !is_loopback_or_wildcard_hex_ip(hex_ip) {
            continue;
        }
        if let Ok(port) = u16::from_str_radix(hex_port, 16) {
            if port > 0 && !ports.contains(&port) {
                ports.push(port);
            }
        }
    }
    ports
}

fn is_loopback_or_wildcard_hex_ip(hex_ip: &str) -> bool {
    let upper = hex_ip.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "0100007F" // 127.0.0.1 little-endian
            | "7F000001" // 127.0.0.1 big-endian
            | "00000000" // 0.0.0.0
            | "00000000000000000000000001000000" // ::1 little-endian word order
            | "00000000000000000000000000000001" // ::1 big-endian
            | "0000000000000000FFFF00000100007F" // ::ffff:127.0.0.1
            | "00000000000000000000000000000000" // ::
    )
}

/// Scans `proc_root` (`/proc` on Linux; safely returns `[]` if `proc_root` is not a directory)
/// for running `language_server` processes owned by `target_uid`.
pub fn discover_local_servers_in(
    proc_root: &Path,
    target_uid: Option<u32>,
) -> Vec<LocalLanguageServer> {
    if !proc_root.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(proc_root) else {
        return Vec::new();
    };

    let tcp4 = fs::read_to_string(proc_root.join("net/tcp")).unwrap_or_default();
    let tcp6 = fs::read_to_string(proc_root.join("net/tcp6")).unwrap_or_default();

    let mut servers = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let fname = entry.file_name();
        let Some(fname_str) = fname.to_str() else {
            continue;
        };
        if !fname_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(pid) = fname_str.parse::<u32>() else {
            continue;
        };

        let proc_dir = entry.path();
        if let Some(expected_uid) = target_uid {
            #[cfg(unix)]
            {
                let Ok(meta) = fs::metadata(&proc_dir) else {
                    continue;
                };
                if meta.uid() != expected_uid {
                    continue;
                }
            }
            #[cfg(not(unix))]
            {
                let _ = expected_uid;
            }
        }

        let Ok(cmdline_bytes) = fs::read(proc_dir.join("cmdline")) else {
            continue;
        };
        let Some(parsed_args) = parse_proc_cmdline(&cmdline_bytes) else {
            continue;
        };

        let mut inodes = HashSet::new();
        if let Ok(fd_entries) = fs::read_dir(proc_dir.join("fd")) {
            for fd_entry in fd_entries.filter_map(Result::ok) {
                if let Ok(link_target) = fs::read_link(fd_entry.path()) {
                    if let Some(target_str) = link_target.to_str() {
                        if let Some(inode) = parse_socket_inode_link(target_str) {
                            inodes.insert(inode);
                        }
                    }
                }
            }
        }

        let mut ports = parse_proc_net_tcp(&tcp4, &inodes);
        for p6 in parse_proc_net_tcp(&tcp6, &inodes) {
            if !ports.contains(&p6) {
                ports.push(p6);
            }
        }
        if let Some(ext_port) = parsed_args.extension_server_port {
            if ext_port > 0 && !ports.contains(&ext_port) {
                ports.push(ext_port);
            }
        }

        if !ports.is_empty() {
            servers.push(LocalLanguageServer {
                pid,
                csrf_token: parsed_args.csrf_token,
                ports,
            });
        }
    }

    servers
}

#[derive(Debug, Clone)]
struct RpcUserStatusSummary {
    email: Option<String>,
    plan_tier: Option<String>,
    gemini_pool: Option<(f64, Option<(i64, String)>)>,
    third_party_pool: Option<(f64, Option<(i64, String)>)>,
    fallback_buckets: Vec<UsageBucket>,
}

fn query_local_rpc_status(ctx: &FetchContext) -> Option<RpcUserStatusSummary> {
    let body = r#"{"metadata":{"ideName":"antigravity","extensionName":"antigravity","locale":"en"}}"#;

    // 1. Explicit test override URL if provided
    if let Some(ref override_url) = ctx.endpoints.agy_rpc_url {
        let headers = [
            ("Connect-Protocol-Version", "1"),
            ("X-Codeium-Csrf-Token", "test-csrf-token"),
        ];
        if let Ok(resp) = ctx.http.post_loopback_json(override_url, &headers, body) {
            if resp.is_success() {
                if let Some(parsed) = parse_rpc_user_status(&resp.body, ctx.now, &Local) {
                    return Some(parsed);
                }
            }
        }
    }

    // 2. Try Antigravity 2.x daemon hub endpoints (`~/.gemini/antigravity/daemon/hub_<port>.json`)
    //    verified against live `/proc/<pid>` ownership for direct 1st-shot HTTP port connection.
    let euid = current_euid();
    for hub in discover_daemon_hub_servers(ctx.home_dir.as_deref(), &ctx.proc_root, euid) {
        let headers = [
            ("Connect-Protocol-Version", "1"),
            ("X-Codeium-Csrf-Token", hub.csrf_token.as_str()),
        ];
        let mut attempts: Vec<(&str, u16)> = Vec::new();
        if let Some(hp) = hub.http_port {
            attempts.push(("http", hp));
        }
        if let Some(sp) = hub.https_port {
            attempts.push(("https", sp));
        }
        for (scheme, port) in attempts {
            let url = format!(
                "{scheme}://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/GetUserStatus"
            );
            if let Ok(resp) = ctx.http.post_loopback_json(&url, &headers, body) {
                if resp.is_success() {
                    if let Some(parsed) = parse_rpc_user_status(&resp.body, ctx.now, &Local) {
                        return Some(parsed);
                    }
                }
            }
        }
    }

    // 3. Fallback: Discover local `/proc` servers via `/proc/<pid>/cmdline` + `/proc/net/tcp{,6}`
    let servers = discover_local_servers_in(&ctx.proc_root, euid);
    for srv in servers {
        let headers = [
            ("Connect-Protocol-Version", "1"),
            ("X-Codeium-Csrf-Token", srv.csrf_token.as_str()),
        ];
        for port in srv.ports {
            for scheme in ["http", "https"] {
                let url = format!(
                    "{scheme}://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/GetUserStatus"
                );
                if let Ok(resp) = ctx.http.post_loopback_json(&url, &headers, body) {
                    if resp.is_success() {
                        if let Some(parsed) = parse_rpc_user_status(&resp.body, ctx.now, &Local) {
                            return Some(parsed);
                        }
                    }
                }
            }
        }
    }

    None
}

fn parse_rpc_user_status<T: TimeZone>(
    raw_json: &str,
    now: DateTime<Utc>,
    tz: &T,
) -> Option<RpcUserStatusSummary>
where
    T::Offset: std::fmt::Display,
{
    let root: serde_json::Value = serde_json::from_str(raw_json).ok()?;
    let user_status = root.get("userStatus").unwrap_or(&root);

    let email = user_status
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    let raw_tier = user_status
        .pointer("/userTier/name")
        .and_then(|v| v.as_str())
        .or_else(|| {
            user_status
                .pointer("/planStatus/planInfo/teamsTier")
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            user_status
                .pointer("/planStatus/planInfo/planName")
                .and_then(|v| v.as_str())
        });
    let plan_tier = raw_tier.and_then(normalize_agy_tier_badge);

    let mut gemini_pool: Option<(f64, Option<(i64, String)>)> = None;
    let mut third_party_pool: Option<(f64, Option<(i64, String)>)> = None;

    if let Some(configs) = user_status
        .pointer("/cascadeModelConfigData/clientModelConfigs")
        .and_then(|v| v.as_array())
    {
        for cfg in configs {
            let label = cfg
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let model_code = cfg
                .pointer("/modelOrAlias/model")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_ascii_lowercase();

            let Some(quota_info) = cfg.get("quotaInfo") else {
                continue;
            };
            let rem_frac = quota_info
                .get("remainingFraction")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0)
                .clamp(0.0, 1.0);
            let reset_norm = quota_info
                .get("resetTime")
                .and_then(|v| v.as_str())
                .and_then(|r| reset::normalize_reset(r, now, tz));

            let is_gemini = label.contains("gemini") || model_code.contains("gemini");
            let is_3p = label.contains("claude")
                || label.contains("gpt")
                || model_code.contains("claude")
                || model_code.contains("gpt");

            if is_gemini {
                // Keep the most constrained (lowest remainingFraction) pool in the family
                if gemini_pool.as_ref().is_none_or(|(prev, _)| rem_frac < *prev) {
                    gemini_pool = Some((rem_frac, reset_norm));
                }
            } else if is_3p && third_party_pool.as_ref().is_none_or(|(prev, _)| rem_frac < *prev)
            {
                third_party_pool = Some((rem_frac, reset_norm));
            }
        }
    }

    let mut fallback_buckets = Vec::new();
    if let Some((rem, ref rst)) = gemini_pool {
        fallback_buckets.push(UsageBucket::from_remaining_fraction(
            "gemini-weekly",
            "Gemini · Weekly",
            "Gemini wk",
            BucketLayout::FullRow,
            true,
            rem,
            rst.clone(),
        ));
    }
    if let Some((rem, ref rst)) = third_party_pool {
        fallback_buckets.push(UsageBucket::from_remaining_fraction(
            "3p-weekly",
            "Claude & GPT · Weekly",
            "Claude/GPT wk",
            BucketLayout::FullRow,
            fallback_buckets.is_empty(),
            rem,
            rst.clone(),
        ));
    }

    if email.is_none() && fallback_buckets.is_empty() {
        return None;
    }

    Some(RpcUserStatusSummary {
        email,
        plan_tier,
        gemini_pool,
        third_party_pool,
        fallback_buckets,
    })
}

fn overlay_rpc_on_buckets(buckets: &mut [UsageBucket], rpc: &RpcUserStatusSummary) {
    if let Some((rem, ref rst)) = rpc.gemini_pool {
        if let Some(target) = buckets
            .iter_mut()
            .find(|b| {
                b.id.starts_with("gemini-")
                    && b.resets_at_epoch == rst.as_ref().map(|(e, _)| *e)
            })
        {
            let updated = UsageBucket::from_remaining_fraction(
                target.id.clone(),
                target.label.clone(),
                target.short_label.clone(),
                target.layout_hint,
                target.is_primary,
                rem,
                rst.clone().or_else(|| {
                    target
                        .resets_at_epoch
                        .zip(target.resets_at_iso.clone())
                }),
            );
            *target = updated;
        }
    }
    if let Some((rem, ref rst)) = rpc.third_party_pool {
        if let Some(target) = buckets
            .iter_mut()
            .find(|b| {
                b.id.starts_with("3p-") && b.resets_at_epoch == rst.as_ref().map(|(e, _)| *e)
            })
        {
            let updated = UsageBucket::from_remaining_fraction(
                target.id.clone(),
                target.label.clone(),
                target.short_label.clone(),
                target.layout_hint,
                target.is_primary,
                rem,
                rst.clone().or_else(|| {
                    target
                        .resets_at_epoch
                        .zip(target.resets_at_iso.clone())
                }),
            );
            *target = updated;
        }
    }
}

fn normalize_agy_tier_badge(raw: &str) -> Option<String> {
    let clean = raw.trim();
    if clean.is_empty() {
        return None;
    }
    let lower = clean.to_ascii_lowercase();
    if lower.contains("ultra") {
        Some("Ultra".to_string())
    } else if lower.contains("pro") {
        Some("Pro".to_string())
    } else if lower.contains("enterprise") {
        Some("Enterprise".to_string())
    } else if lower.contains("team") {
        Some("Team".to_string())
    } else if lower.contains("free") || lower.contains("individual") {
        None
    } else {
        Some(clean.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    const SAMPLE_AGY_4BUCKET_JSON: &str = r#"{"conversation_id":"","status":"SUCCESS","response":"Gemini Models\tWeekly Limit Remaining\t84%\t2026-11-07T15:56:12Z\nGemini Models\tFive Hour Limit Remaining\t93%\t2026-11-06T01:53:02Z\nClaude and GPT models\tWeekly Limit Remaining\t100%\t2026-11-12T23:14:42Z\nClaude and GPT models\tFive Hour Limit Remaining\t75%\t2026-11-06T04:14:42Z\n","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":0.8415147662162781,"reset_time":"2026-11-07T15:56:12Z"},{"id":"gemini-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":0.9305146932601929,"reset_time":"2026-11-06T01:53:02Z"}]},{"name":"Claude and GPT models","buckets":[{"id":"3p-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":1,"reset_time":"2026-11-12T23:14:42Z"},{"id":"3p-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":0.75,"reset_time":"2026-11-06T04:14:42Z"}]}]}}}"#;

    #[test]
    fn test_parse_all_4_agy_buckets_and_labels() {
        let now = Utc.with_ymd_and_hms(2026, 11, 6, 0, 0, 0).unwrap();
        let pst = FixedOffset::west_opt(8 * 3600).unwrap();

        let buckets =
            parse_agy_4buckets(SAMPLE_AGY_4BUCKET_JSON, now, &pst).expect("parse 4 buckets");
        assert_eq!(buckets.len(), 4);

        assert_eq!(buckets[0].id, "gemini-weekly");
        assert_eq!(buckets[0].label, "Gemini · Weekly");
        assert_eq!(buckets[0].layout_hint, BucketLayout::FullRow);
        assert!(buckets[0].is_primary);
        assert_eq!(buckets[0].used_percent, 16);
        assert_eq!(buckets[0].remaining_percent, 84);

        assert_eq!(buckets[1].id, "gemini-5h");
        assert_eq!(buckets[1].label, "Gemini · 5h");
        assert!(!buckets[1].is_primary);
        assert_eq!(buckets[1].used_percent, 7);
        assert_eq!(buckets[1].remaining_percent, 93);

        assert_eq!(buckets[2].id, "3p-weekly");
        assert_eq!(buckets[2].label, "Claude & GPT · Weekly");
        assert_eq!(buckets[2].used_percent, 0);
        assert_eq!(buckets[2].remaining_percent, 100);

        assert_eq!(buckets[3].id, "3p-5h");
        assert_eq!(buckets[3].label, "Claude & GPT · 5h");
        assert_eq!(buckets[3].used_percent, 25);
        assert_eq!(buckets[3].remaining_percent, 75);
    }

    #[test]
    fn test_resolve_agy_cli_app_data_dir_fixes_gemini_bug() {
        let tmp = std::env::temp_dir().join(format!("gas-agy-dir-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let gemini_dir = tmp.join(".gemini");
        fs::create_dir_all(gemini_dir.join("antigravity")).unwrap();

        // When only `.gemini/antigravity` exists (no `antigravity-cli`), MUST return None!
        assert_eq!(resolve_agy_cli_app_data_dir(&gemini_dir), None);

        // When `.gemini/antigravity-cli` exists, returns `.gemini/antigravity-cli`
        let cli_dir = gemini_dir.join("antigravity-cli");
        fs::create_dir_all(&cli_dir).unwrap();
        assert_eq!(resolve_agy_cli_app_data_dir(&gemini_dir), Some(cli_dir));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_pure_rust_proc_cmdline_and_net_tcp_parsers() {
        let cmdline = b"/opt/antigravity/language_server_linux_x64\0--csrf_token\0sec-tok-999\0--extension_server_port=42100\0";
        let parsed = parse_proc_cmdline(cmdline).expect("parse language_server cmdline");
        assert_eq!(parsed.csrf_token, "sec-tok-999");
        assert_eq!(parsed.extension_server_port, Some(42100));

        // Unrelated binary must be ignored
        assert!(parse_proc_cmdline(b"/usr/bin/python3\0server.py\0").is_none());

        // Port 0xA475 = 42101 on 127.0.0.1 (0100007F), state 0A (LISTEN), inode 987654
        let sample_tcp = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:A475 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 987654 1 0000000000000000 100 0 0 10 0
   1: 0100007F:1F90 0100007F:C350 01 00000000:00000000 00:00000000 00000000  1000        0 987654 1 0000000000000000 20 4 30 10 -1
";
        let mut inodes = HashSet::new();
        inodes.insert(987654);
        let ports = parse_proc_net_tcp(sample_tcp, &inodes);
        assert_eq!(ports, vec![42101]);
    }

    #[test]
    fn test_mock_proc_and_loopback_get_user_status_rpc_end_to_end() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();

        let srv_handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req_buf = [0u8; 2048];
            let n = stream.read(&mut req_buf).unwrap_or(0);
            let req_text = String::from_utf8_lossy(&req_buf[..n]).to_ascii_lowercase();
            assert!(req_text.contains("x-codeium-csrf-token: mock-csrf-abc"));
            assert!(req_text.contains("connect-protocol-version: 1"));

            let body = r#"{"userStatus":{"email":"pilot@example.com","userTier":{"name":"Google AI Pro"},"cascadeModelConfigData":{"clientModelConfigs":[{"label":"Gemini 2.5 Pro","modelOrAlias":{"model":"MODEL_GEMINI_2_5_PRO"},"quotaInfo":{"remainingFraction":0.75,"resetTime":"2026-11-11T12:00:00Z"}},{"label":"Claude 4.5 Sonnet","modelOrAlias":{"model":"MODEL_CLAUDE_4_5_SONNET"},"quotaInfo":{"remainingFraction":0.60,"resetTime":"2026-11-11T04:00:00Z"}}]}}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
        });

        // Build mock `/proc` tree in temp dir (works identically on macOS and Linux!)
        let mock_proc = std::env::temp_dir().join(format!(
            "gas-mock-proc-{}-{}",
            std::process::id(),
            port
        ));
        let _ = fs::remove_dir_all(&mock_proc);
        fs::create_dir_all(mock_proc.join("4242/fd")).unwrap();
        fs::create_dir_all(mock_proc.join("net")).unwrap();

        fs::write(
            mock_proc.join("4242/cmdline"),
            b"/opt/antigravity/language_server_linux\0--csrf_token=mock-csrf-abc\0",
        )
        .unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("socket:[555111]", mock_proc.join("4242/fd/3")).unwrap();
        }
        fs::write(
            mock_proc.join("net/tcp"),
            format!(
                "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:{:04X} 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 555111 1 0000000000000000 100 0 0 10 0\n",
                port
            ),
        )
        .unwrap();

        let mut ctx = FetchContext::from_config(&crate::config::Config::default());
        ctx.proc_root = mock_proc.clone();

        let provider = AntigravityProvider::new();
        let desc = AccountDescriptor {
            provider_id: "agy".to_string(),
            id: "agy:default".to_string(),
            label: "Antigravity".to_string(),
            dedup_key: Some("agy-system-default".to_string()),
            path: None,
            source_label: Some("local language_server".to_string()),
            binary: Some("/nonexistent/agy-binary".to_string()),
            is_system_default: true,
            poll_mode: PollMode::Active,
            login_command: Some("agy auth login".to_string()),
            switch_command: None,
        };

        assert_eq!(provider.effective_ttl_secs(&desc, &ctx), 60);
        let outcome = provider.fetch_account(&desc, None, &ctx);
        srv_handle.join().unwrap();
        let _ = fs::remove_dir_all(&mock_proc);

        match outcome {
            AccountFetchOutcome::Success(rep) => {
                assert_eq!(rep.id, "agy:pilot@example.com");
                assert_eq!(rep.label, "pilot@example.com");
                assert_eq!(rep.plan_tier.as_deref(), Some("Pro"));
                assert_eq!(rep.buckets.len(), 2);
                assert_eq!(rep.buckets[0].used_percent, 25);
                assert_eq!(rep.buckets[1].used_percent, 40);
            }
            other => panic!("expected Success, got {:?}", other),
        }
    }

    #[test]
    fn test_linux_antigravity_2_cmdline_and_daemon_hub_discovery_rejects_stale_pid() {
        let agy2_cmdline = b"/home/user/.local/opt/antigravity-app/resources/bin/language_server\0--standalone\0--override_ide_name\0antigravity\0--subclient_type\0hub\0--app_data_dir\0antigravity\0--csrf_token\0hub-csrf-777\0";
        let parsed = parse_proc_cmdline(agy2_cmdline).expect("parse Antigravity 2.0 cmdline");
        assert_eq!(parsed.csrf_token, "hub-csrf-777");
        assert_eq!(parsed.extension_server_port, None);

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let srv_handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req_buf = [0u8; 2048];
            let n = stream.read(&mut req_buf).unwrap_or(0);
            let req_text = String::from_utf8_lossy(&req_buf[..n]).to_ascii_lowercase();
            assert!(req_text.contains("x-codeium-csrf-token: hub-csrf-777"));

            let body = r#"{"userStatus":{"email":"hub@example.com","userTier":{"name":"Google AI Ultra"},"cascadeModelConfigData":{"clientModelConfigs":[{"label":"Gemini 4 Carbon","modelOrAlias":{"model":"MODEL_PLACEHOLDER_M75"},"quotaInfo":{"remainingFraction":0.90,"resetTime":"2026-11-11T12:00:00Z"}},{"label":"Claude Sonnet 5.5","modelOrAlias":{"model":"MODEL_PLACEHOLDER_M65"},"quotaInfo":{"remainingFraction":1.0,"resetTime":"2026-11-11T12:00:00Z"}}]}}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
        });

        let root = std::env::temp_dir().join(format!(
            "gas-agy2-hub-{}-{}",
            std::process::id(),
            port
        ));
        let _ = fs::remove_dir_all(&root);
        let mock_home = root.join("home");
        let mock_proc = root.join("proc");
        let daemon_dir = mock_home.join(".gemini/antigravity/daemon");
        fs::create_dir_all(&daemon_dir).unwrap();
        fs::create_dir_all(mock_proc.join("2062235")).unwrap();
        fs::create_dir_all(mock_proc.join("999999")).unwrap();

        fs::write(mock_proc.join("2062235/cmdline"), agy2_cmdline).unwrap();
        // Recycled PID 999999 running an unrelated binary must be rejected!
        fs::write(mock_proc.join("999999/cmdline"), b"/usr/bin/python3\0app.py\0").unwrap();

        // Stale hub for dead PID 1811058 + recycled PID 999999 + live hub for 2062235
        fs::write(
            daemon_dir.join("hub_38383.json"),
            r#"{"csrfToken":"dead-csrf","httpPort":38383,"httpsPort":38384,"pid":1811058}"#,
        )
        .unwrap();
        fs::write(
            daemon_dir.join("hub_39393.json"),
            r#"{"csrfToken":"recycled-csrf","httpPort":39393,"httpsPort":39394,"pid":999999}"#,
        )
        .unwrap();
        fs::write(
            daemon_dir.join(format!("hub_{port}.json")),
            format!(
                r#"{{"csrfToken":"hub-csrf-777","httpPort":{port},"httpsPort":65530,"lsVersion":"2.31.0","pid":2062235}}"#
            ),
        )
        .unwrap();

        let hubs = discover_daemon_hub_servers(Some(&mock_home), &mock_proc, current_euid());
        assert_eq!(hubs.len(), 1);
        assert_eq!(hubs[0].pid, 2062235);
        assert_eq!(hubs[0].http_port, Some(port));

        let mut ctx = FetchContext::from_config(&crate::config::Config::default());
        ctx.home_dir = Some(mock_home);
        ctx.proc_root = mock_proc;

        let provider = AntigravityProvider::new();
        let desc = AccountDescriptor {
            provider_id: "agy".to_string(),
            id: "agy:default".to_string(),
            label: "Antigravity".to_string(),
            dedup_key: Some("agy-system-default".to_string()),
            path: None,
            source_label: Some("local language_server".to_string()),
            binary: None,
            is_system_default: true,
            poll_mode: PollMode::Active,
            login_command: Some("agy auth login".to_string()),
            switch_command: None,
        };

        let outcome = provider.fetch_account(&desc, None, &ctx);
        srv_handle.join().unwrap();
        let _ = fs::remove_dir_all(&root);

        match outcome {
            AccountFetchOutcome::Success(rep) => {
                assert_eq!(rep.id, "agy:hub@example.com");
                assert_eq!(rep.plan_tier.as_deref(), Some("Ultra"));
                assert_eq!(rep.buckets[0].used_percent, 10);
                assert_eq!(rep.buckets[1].used_percent, 0);
            }
            other => panic!("expected Success, got {:?}", other),
        }
    }

    #[test]
    fn test_secondary_manual_account_does_not_query_local_rpc() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();

        let mut ctx = FetchContext::from_config(&crate::config::Config::default());
        ctx.endpoints.agy_rpc_url = Some(format!(
            "http://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/GetUserStatus"
        ));

        let provider = AntigravityProvider::new();
        let secondary_desc = AccountDescriptor {
            provider_id: "agy".to_string(),
            id: "agy:secondary".to_string(),
            label: "secondary@example.com".to_string(),
            dedup_key: None,
            path: None,
            source_label: Some("~/.gemini-secondary".to_string()),
            binary: Some("/nonexistent/agy-secondary-bin".to_string()),
            is_system_default: false,
            poll_mode: PollMode::Active,
            login_command: Some("agy auth login".to_string()),
            switch_command: None,
        };

        let outcome = provider.fetch_account(&secondary_desc, None, &ctx);
        // Must NOT have connected to the primary IDE's local RPC server!
        assert!(listener.accept().is_err());
        assert!(matches!(outcome, AccountFetchOutcome::Error { .. }));
    }
}
