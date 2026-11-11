//! Windsurf (`windsurf`) usage provider:
//! 1. Tier 1 (Linux active IDE): Scans `/proc/<pid>/cmdline` (`ctx.proc_root`) for `language_server`
//!    (`language_server_linux*`) processes whose cmdline contains `windsurf` or `.codeium/windsurf`
//!    (strictly distinct from Antigravity!) and queries local Connect-RPC
//!    `exa.language_server_pb.LanguageServerService/GetUserStatus` for plan tier and prompt/flow
//!    credit buckets.
//! 2. Tier 2 (Read-only SQLite fallback): Opens `~/.config/Windsurf/User/globalStorage/state.vscdb`
//!    (or manual `account.path`) via bundled `rusqlite` (`file:<uri>?mode=ro&immutable=1` +
//!    `PRAGMA query_only = ON`) inspecting `windsurfAuthStatus` / `codeium.windsurf` JSON metadata
//!    cleanly without ever locking Windsurf or exposing `apiKey` (`sk-ws-*`). Omits cleanly when
//!    Windsurf is not installed.

use crate::cache::{apply_bucket_time_decay, CachedAccountState};
use crate::config::{
    abbreviate_home, discover_provider_locations, expand_tilde_with_home, slugify_id,
    ManualAccountConfig, PollMode,
};
use crate::model::{AccountReport, AccountState, BucketLayout, CreditSummary, UsageBucket};
use crate::providers::antigravity::{
    parse_proc_net_tcp, parse_socket_inode_link, LocalLanguageServer, ProcLanguageServerArgs,
};
use crate::providers::{
    AccountDescriptor, AccountFetchOutcome, FetchContext, ProviderMeta, UsageProvider,
};
use crate::reset;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Local, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

pub struct WindsurfProvider;

impl WindsurfProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindsurfProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageProvider for WindsurfProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            id: "windsurf",
            display_name: "Windsurf",
            icon_name: "provider-windsurf",
            dashboard_url: "https://windsurf.com/subscription",
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
            let source_label = expanded.as_deref().map(|p| abbreviate_home(p, home));
            let id_slug = if m.id.trim().is_empty() {
                format!("manual-{}", idx + 1)
            } else {
                slugify_id(&m.id)
            };

            out.push(AccountDescriptor {
                provider_id: "windsurf".to_string(),
                id: format!("windsurf:{id_slug}"),
                label: sanitize_windsurf_label(Some(&m.display_label()), None, None),
                dedup_key: expanded.as_ref().map(|p| p.to_string_lossy().into_owned()),
                path: expanded,
                source_label,
                binary: m.binary.clone(),
                is_system_default: idx == 0 && !auto_discover,
                poll_mode: m.poll_mode,
                login_command: Some("windsurf".to_string()),
                switch_command: m.switch_command.clone(),
            });
        }

        if auto_discover {
            let locs = discover_provider_locations(
                "windsurf",
                home,
                ctx.xdg_config_dir.as_deref(),
            );
            let live_server = has_local_windsurf_server(ctx);

            if let Some(first_loc) = locs.first() {
                out.push(AccountDescriptor {
                    provider_id: "windsurf".to_string(),
                    id: format!("windsurf:{}", first_loc.suggested_id),
                    label: "Windsurf".to_string(),
                    dedup_key: Some(first_loc.path.to_string_lossy().into_owned()),
                    path: Some(first_loc.path.clone()),
                    source_label: Some(first_loc.source_label.clone()),
                    binary: None,
                    is_system_default: true,
                    poll_mode: first_loc.poll_mode,
                    login_command: Some("windsurf".to_string()),
                    switch_command: None,
                });

                for extra in locs.iter().skip(1) {
                    out.push(AccountDescriptor {
                        provider_id: "windsurf".to_string(),
                        id: format!("windsurf:{}", extra.suggested_id),
                        label: "Windsurf".to_string(),
                        dedup_key: Some(extra.path.to_string_lossy().into_owned()),
                        path: Some(extra.path.clone()),
                        source_label: Some(extra.source_label.clone()),
                        binary: None,
                        is_system_default: false,
                        poll_mode: extra.poll_mode,
                        login_command: Some("windsurf".to_string()),
                        switch_command: None,
                    });
                }
            } else if live_server {
                out.push(AccountDescriptor {
                    provider_id: "windsurf".to_string(),
                    id: "windsurf:default".to_string(),
                    label: "Windsurf".to_string(),
                    dedup_key: Some("windsurf-local-rpc".to_string()),
                    path: None,
                    source_label: Some("local Windsurf server".to_string()),
                    binary: None,
                    is_system_default: true,
                    poll_mode: PollMode::Active,
                    login_command: Some("windsurf".to_string()),
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
        let is_auto = account.id == "windsurf:default" || account.id.starts_with("windsurf:default-");

        // Tier 1: Local Windsurf Connect-RPC (`GetUserStatus`) for the primary system-default IDE
        if account.is_system_default {
            if let Some(rpc_summary) = query_local_windsurf_rpc(ctx) {
                let resolved_label = sanitize_windsurf_label(
                    Some(&account.label),
                    rpc_summary.email.as_deref(),
                    rpc_summary.name.as_deref(),
                );
                let resolved_id = resolve_windsurf_account_id(
                    account,
                    rpc_summary.email.as_deref().or(rpc_summary.name.as_deref()),
                );
                return AccountFetchOutcome::Success(AccountReport {
                    id: resolved_id,
                    label: resolved_label,
                    source_label: account.source_label.clone(),
                    is_system_default: account.is_system_default,
                    is_active: false,
                    plan_tier: rpc_summary
                        .plan_tier
                        .or_else(|| cached.and_then(|c| c.plan_tier.clone())),
                    state: AccountState::ActiveOk,
                    is_stale: false,
                    status_note: None,
                    extra_info: rpc_summary.extra_info,
                    error: None,
                    login_command: account.login_command.clone(),
                    updated_at_epoch: Some(now_epoch),
                    buckets: rpc_summary.buckets,
                    credits: rpc_summary.credits,
                    sparkline: Vec::new(),
                });
            }
        }

        // Tier 2: Read-only SQLite `state.vscdb` (or JSON metadata file) fallback
        if let Some(ref db_path) = account.path {
            match read_windsurf_vscdb_metadata(db_path, ctx.now, &Local) {
                Ok(Some(meta)) => {
                    let resolved_label = sanitize_windsurf_label(
                        Some(&account.label),
                        meta.email.as_deref(),
                        meta.name.as_deref(),
                    );
                    let resolved_id = resolve_windsurf_account_id(
                        account,
                        meta.email.as_deref().or(meta.name.as_deref()),
                    );
                    let plan_tier = meta
                        .plan_tier
                        .or_else(|| cached.and_then(|c| c.plan_tier.clone()));

                    if !meta.buckets.is_empty() {
                        return AccountFetchOutcome::Success(AccountReport {
                            id: resolved_id,
                            label: resolved_label,
                            source_label: account.source_label.clone(),
                            is_system_default: account.is_system_default,
                            is_active: false,
                            plan_tier,
                            state: AccountState::ActiveOk,
                            is_stale: false,
                            status_note: None,
                            extra_info: meta.extra_info,
                            error: None,
                            login_command: account.login_command.clone(),
                            updated_at_epoch: Some(now_epoch),
                            buckets: meta.buckets,
                            credits: meta.credits,
                            sparkline: Vec::new(),
                        });
                    }

                    let mut buckets = cached.map(|c| c.buckets.clone()).unwrap_or_default();
                    apply_bucket_time_decay(&mut buckets, now_epoch);

                    return AccountFetchOutcome::Passive(AccountReport {
                        id: resolved_id,
                        label: resolved_label,
                        source_label: account.source_label.clone(),
                        is_system_default: account.is_system_default,
                        is_active: false,
                        plan_tier,
                        state: AccountState::PassiveCached,
                        is_stale: !buckets.is_empty(),
                        status_note: Some("Updates when Windsurf runs".to_string()),
                        extra_info: meta.extra_info,
                        error: None,
                        login_command: account.login_command.clone(),
                        updated_at_epoch: cached.and_then(|c| c.last_success_epoch),
                        buckets,
                        credits: cached.and_then(|c| c.credits.clone()),
                        sparkline: cached.map(|c| c.sparkline.clone()).unwrap_or_default(),
                    });
                }
                Ok(None) => {
                    if is_auto {
                        return AccountFetchOutcome::Omit;
                    }
                    return AccountFetchOutcome::SignedOut {
                        message: "Windsurf session is signed out".to_string(),
                        login_command: account.login_command.clone(),
                        label: Some(account.label.clone()),
                        plan_tier: None,
                    };
                }
                Err(err) => {
                    if is_auto && !db_path.exists() {
                        return AccountFetchOutcome::Omit;
                    }
                    return AccountFetchOutcome::Error {
                        message: format!("{err:#}"),
                    };
                }
            }
        }

        if is_auto {
            AccountFetchOutcome::Omit
        } else {
            AccountFetchOutcome::SignedOut {
                message: "Windsurf is not running and state.vscdb was not found".to_string(),
                login_command: account.login_command.clone(),
                label: Some(account.label.clone()),
                plan_tier: None,
            }
        }
    }
}

fn current_euid() -> Option<u32> {
    #[cfg(unix)]
    {
        // SAFETY: `libc::geteuid()` is a pure POSIX syscall with no pointer arguments.
        Some(unsafe { libc::geteuid() })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn has_local_windsurf_server(ctx: &FetchContext) -> bool {
    ctx.endpoints.windsurf_rpc_url.is_some()
        || !discover_windsurf_servers_in(&ctx.proc_root, current_euid()).is_empty()
}

/// Pure-Rust parser for Windsurf `/proc/<pid>/cmdline`:
/// - `args[0]` MUST contain `"language_server"` (e.g. `language_server_linux_x64`),
/// - At least one argument MUST contain `"windsurf"` or `".codeium/windsurf"`,
/// - MUST NOT contain `"antigravity"` anywhere in `args`.
pub fn parse_windsurf_proc_cmdline(raw: &[u8]) -> Option<ProcLanguageServerArgs> {
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

    let has_antigravity = args
        .iter()
        .any(|a| a.to_ascii_lowercase().contains("antigravity"));
    if has_antigravity {
        return None;
    }

    let is_windsurf = args.iter().any(|a| {
        let l = a.to_ascii_lowercase();
        l.contains("windsurf") || l.contains(".codeium/windsurf")
    });
    if !is_windsurf {
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

/// Scans `proc_root` (`/proc`) for running Windsurf `language_server_linux*` processes owned by `target_uid`.
pub fn discover_windsurf_servers_in(
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
        let Some(parsed_args) = parse_windsurf_proc_cmdline(&cmdline_bytes) else {
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
pub struct WindsurfStatusSummary {
    pub email: Option<String>,
    pub name: Option<String>,
    pub plan_tier: Option<String>,
    pub extra_info: Option<String>,
    pub buckets: Vec<UsageBucket>,
    pub credits: Option<CreditSummary>,
}

fn query_local_windsurf_rpc(ctx: &FetchContext) -> Option<WindsurfStatusSummary> {
    let body =
        r#"{"metadata":{"ideName":"windsurf","extensionName":"windsurf","locale":"en"}}"#;

    if let Some(ref override_url) = ctx.endpoints.windsurf_rpc_url {
        let headers = [
            ("Connect-Protocol-Version", "1"),
            ("X-Codeium-Csrf-Token", "test-windsurf-csrf"),
        ];
        if let Ok(resp) = ctx.http.post_loopback_json(override_url, &headers, body) {
            if resp.is_success() {
                if let Some(parsed) = parse_windsurf_user_status_json(&resp.body, ctx.now, &Local) {
                    return Some(parsed);
                }
            }
        }
    }

    for srv in discover_windsurf_servers_in(&ctx.proc_root, current_euid()) {
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
                        if let Some(parsed) =
                            parse_windsurf_user_status_json(&resp.body, ctx.now, &Local)
                        {
                            return Some(parsed);
                        }
                    }
                }
            }
        }
    }

    None
}

pub fn parse_windsurf_user_status_json<T: TimeZone>(
    raw_json: &str,
    now: DateTime<Utc>,
    tz: &T,
) -> Option<WindsurfStatusSummary>
where
    T::Offset: std::fmt::Display,
{
    let root: serde_json::Value = serde_json::from_str(raw_json).ok()?;
    parse_windsurf_value(&root, now, tz, true)
}

fn parse_windsurf_value<T: TimeZone>(
    root: &serde_json::Value,
    now: DateTime<Utc>,
    tz: &T,
    require_buckets: bool,
) -> Option<WindsurfStatusSummary>
where
    T::Offset: std::fmt::Display,
{
    let user_status = root.get("userStatus").unwrap_or(root);

    let email = extract_clean_string(user_status, &["email", "userEmail"]);
    let name = extract_clean_string(user_status, &["name", "userName", "username"]);

    let plan_status = user_status
        .get("planStatus")
        .or_else(|| root.get("planStatus"));
    let plan_info = plan_status
        .and_then(|ps| ps.get("planInfo"))
        .or_else(|| user_status.get("planInfo"))
        .or_else(|| root.get("planInfo"));

    let raw_plan = plan_info
        .and_then(|pi| extract_clean_string(pi, &["planName", "teamsTier", "planTier"]))
        .or_else(|| extract_clean_string(user_status, &["planName", "teamsTier", "plan"]))
        .or_else(|| extract_clean_string(root, &["planName", "teamsTier", "plan"]));
    let plan_tier = raw_plan.as_deref().and_then(normalize_windsurf_plan);

    let reset = plan_status
        .and_then(|ps| extract_clean_string(ps, &["planEnd", "resetTime", "resetsAt"]))
        .and_then(|s| reset::normalize_reset(&s, now, tz));

    let monthly_prompt = plan_info
        .and_then(|pi| json_f64(pi.get("monthlyPromptCredits")))
        .or_else(|| plan_status.and_then(|ps| json_f64(ps.get("monthlyPromptCredits"))));
    let avail_prompt = plan_status
        .and_then(|ps| json_f64(ps.get("availablePromptCredits")))
        .or_else(|| json_f64(user_status.get("availablePromptCredits")));
    let used_prompt = plan_status.and_then(|ps| json_f64(ps.get("usedPromptCredits")));

    let monthly_flow = plan_info
        .and_then(|pi| json_f64(pi.get("monthlyFlowCredits")))
        .or_else(|| plan_status.and_then(|ps| json_f64(ps.get("monthlyFlowCredits"))));
    let avail_flow = plan_status
        .and_then(|ps| json_f64(ps.get("availableFlowCredits")))
        .or_else(|| json_f64(user_status.get("availableFlowCredits")));
    let used_flow = plan_status.and_then(|ps| json_f64(ps.get("usedFlowCredits")));

    let avail_flex = plan_status.and_then(|ps| json_f64(ps.get("availableFlexCredits")));

    let mut buckets = Vec::new();
    let mut extra_parts = Vec::new();

    let has_flow_bucket = monthly_flow.is_some_and(|m| m > 0.0) && (avail_flow.is_some() || used_flow.is_some());
    let layout = if has_flow_bucket {
        BucketLayout::InlinePair
    } else {
        BucketLayout::FullRow
    };

    if let Some(total_p) = monthly_prompt.filter(|&m| m > 0.0) {
        let used_p = if let Some(avail) = avail_prompt {
            (total_p - avail.max(0.0)).clamp(0.0, total_p)
        } else if let Some(u) = used_prompt {
            u.clamp(0.0, total_p)
        } else {
            0.0
        };
        if avail_prompt.is_some() || used_prompt.is_some() {
            let pct = (used_p / total_p) * 100.0;
            let (disp_used, disp_total) = normalize_credit_units(used_p, total_p);
            let info = format!("{disp_used:.0} / {disp_total:.0} prompt credits used");
            extra_parts.push(info.clone());
            buckets.push(
                UsageBucket::new(
                    "prompt-credits",
                    "Prompt Credits",
                    "Prompt",
                    layout,
                    true,
                    pct,
                    reset.clone(),
                )
                .with_window_seconds(Some(2_678_400))
                .with_extra_info(info),
            );
        }
    }

    if let Some(total_f) = monthly_flow.filter(|&m| m > 0.0) {
        if avail_flow.is_some() || used_flow.is_some() {
            let used_f = if let Some(avail) = avail_flow {
                (total_f - avail.max(0.0)).clamp(0.0, total_f)
            } else {
                used_flow.unwrap_or(0.0).clamp(0.0, total_f)
            };
            let pct = (used_f / total_f) * 100.0;
            let (disp_used, disp_total) = normalize_credit_units(used_f, total_f);
            let info = format!("{disp_used:.0} / {disp_total:.0} flow credits used");
            extra_parts.push(info.clone());
            buckets.push(
                UsageBucket::new(
                    "flow-credits",
                    "Flow Credits",
                    "Flow",
                    BucketLayout::InlinePair,
                    buckets.is_empty(),
                    pct,
                    reset.clone(),
                )
                .with_window_seconds(Some(2_678_400))
                .with_extra_info(info),
            );
        }
    }

    // Fallback: Cascade model quota configs if plan credit counters were omitted
    if buckets.is_empty() {
        if let Some(configs) = user_status
            .get("cascadeModelConfigData")
            .and_then(|c| c.get("clientModelConfigs"))
            .and_then(|v| v.as_array())
        {
            for cfg in configs {
                if let Some(rem) = cfg
                    .get("quotaInfo")
                    .and_then(|q| json_f64(q.get("remainingFraction")))
                {
                    let r_ts = cfg
                        .get("quotaInfo")
                        .and_then(|q| q.get("resetTime"))
                        .and_then(|v| v.as_str())
                        .and_then(|s| reset::normalize_reset(s, now, tz));
                    buckets.push(
                        UsageBucket::from_remaining_fraction(
                            "prompt-credits",
                            "Cascade Credits",
                            "Prompt",
                            BucketLayout::FullRow,
                            true,
                            rem,
                            r_ts,
                        )
                        .with_window_seconds(Some(2_678_400)),
                    );
                    break;
                }
            }
        }
    }

    if let Some(flex) = avail_flex.filter(|&f| f > 0.0) {
        let (disp_flex, _) = normalize_credit_units(flex, flex);
        extra_parts.push(format!("{disp_flex:.0} flex credits available"));
    }

    if require_buckets && buckets.is_empty() {
        return None;
    }

    let has_any_identity = email.is_some()
        || name.is_some()
        || plan_tier.is_some()
        || !buckets.is_empty()
        || root
            .get("apiKey")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty());

    if !has_any_identity {
        return None;
    }

    Some(WindsurfStatusSummary {
        email,
        name,
        plan_tier,
        extra_info: if extra_parts.is_empty() {
            None
        } else {
            Some(extra_parts.join(" · "))
        },
        buckets,
        credits: None,
    })
}

fn normalize_credit_units(used: f64, total: f64) -> (f64, f64) {
    // Windsurf's protobuf API encodes 500 credits as `50000` centi-credits on standard plans.
    if total >= 10_000.0 && (total % 100.0).abs() < f64::EPSILON {
        (used / 100.0, total / 100.0)
    } else {
        (used, total)
    }
}

fn json_f64(val: Option<&serde_json::Value>) -> Option<f64> {
    let v = val?;
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
        .filter(|n| n.is_finite())
}

fn extract_clean_string(obj: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for &k in keys {
        if let Some(s) = obj
            .get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !s.starts_with("sk-") && !crate::http::redact_secrets(s).contains("[REDACTED]") {
                return Some(s.to_string());
            }
        }
    }
    None
}

pub fn normalize_windsurf_plan(raw: &str) -> Option<String> {
    let clean = raw.trim();
    if clean.is_empty() {
        return None;
    }
    let lower = clean
        .to_ascii_lowercase()
        .trim_start_matches("teams_tier_")
        .to_string();
    let badge = match lower.as_str() {
        "pro" | "windsurf pro" => "Pro",
        "pro_ultimate" | "pro ultimate" | "ultimate" => "Pro Ultimate",
        "teams" | "team" => "Teams",
        "enterprise" | "enterprise_saas" | "enterprise_self_hosted" => "Enterprise",
        "free" | "individual" | "starter" => "Free",
        _ => return Some(clean.to_string()),
    };
    Some(badge.to_string())
}

/// Opens Windsurf's `state.vscdb` (or manual JSON metadata file) strictly read-only via bundled
/// `rusqlite` (`file:<uri>?mode=ro&immutable=1` + `PRAGMA query_only = ON`) without ever locking
/// Windsurf or surfacing `apiKey` (`sk-ws-*`).
pub fn read_windsurf_vscdb_metadata<T: TimeZone>(
    path: &Path,
    now: DateTime<Utc>,
    tz: &T,
) -> Result<Option<WindsurfStatusSummary>>
where
    T::Offset: std::fmt::Display,
{
    let target = resolve_windsurf_target(path)?;
    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if ext == "json" && !is_sqlite_file(&target) {
        let raw = fs::read_to_string(&target)?;
        let val: serde_json::Value = serde_json::from_str(raw.trim())?;
        return Ok(parse_windsurf_value(&val, now, tz, false));
    }

    let uri = format!(
        "file:{}?mode=ro&immutable=1",
        encode_sqlite_uri_path(&target)
    );
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(&uri, flags)
        .map_err(|e| anyhow!("Failed to open Windsurf state.vscdb read-only: {e}"))?;
    conn.execute_batch("PRAGMA query_only = ON;")
        .map_err(|e| anyhow!("Failed to enforce PRAGMA query_only on Windsurf DB: {e}"))?;

    let mut stmt = conn.prepare(
        "SELECT key, CAST(value AS TEXT) FROM ItemTable \
         WHERE key = 'windsurfAuthStatus' OR key = 'codeium.windsurf' OR key LIKE 'codeium.windsurf%'",
    )?;
    let rows = stmt.query_map([], |row| {
        let k: String = row.get(0)?;
        let v: String = row.get(1)?;
        Ok((k, v))
    })?;

    let mut merged = WindsurfStatusSummary {
        email: None,
        name: None,
        plan_tier: None,
        extra_info: None,
        buckets: Vec::new(),
        credits: None,
    };
    let mut found_authenticated = false;

    for item in rows {
        let (_key, raw_val) = item?;
        let trimmed = raw_val.trim();
        if trimmed.is_empty() || trimmed == "null" || trimmed == "{}" {
            continue;
        }
        if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(summary) = parse_windsurf_value(&json_val, now, tz, false) {
                found_authenticated = true;
                if merged.email.is_none() {
                    merged.email = summary.email;
                }
                if merged.name.is_none() {
                    merged.name = summary.name;
                }
                if merged.plan_tier.is_none() {
                    merged.plan_tier = summary.plan_tier;
                }
                if merged.buckets.is_empty() && !summary.buckets.is_empty() {
                    merged.buckets = summary.buckets;
                    merged.extra_info = summary.extra_info;
                }
            }
        }
    }

    if found_authenticated {
        Ok(Some(merged))
    } else {
        Ok(None)
    }
}

fn resolve_windsurf_target(path: &Path) -> Result<PathBuf> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if path.is_dir() {
        for candidate in [
            path.join("state.vscdb"),
            path.join("User/globalStorage/state.vscdb"),
            path.join("auth.json"),
        ] {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(anyhow!(
        "Windsurf state.vscdb not found at {}",
        path.display()
    ))
}

fn is_sqlite_file(path: &Path) -> bool {
    fs::read(path)
        .map(|b| b.starts_with(b"SQLite format 3\0"))
        .unwrap_or(false)
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

fn sanitize_windsurf_label(
    custom_label: Option<&str>,
    email: Option<&str>,
    name: Option<&str>,
) -> String {
    if let Some(custom) = custom_label.map(str::trim).filter(|s| !s.is_empty()) {
        if custom != "Windsurf"
            && custom != "default"
            && !custom.starts_with("sk-")
            && !crate::http::redact_secrets(custom).contains("[REDACTED]")
        {
            return custom.to_string();
        }
    }
    if let Some(em) = email.map(str::trim).filter(|s| !s.is_empty()) {
        return em.to_string();
    }
    if let Some(nm) = name.map(str::trim).filter(|s| !s.is_empty()) {
        return nm.to_string();
    }
    "Windsurf".to_string()
}

fn resolve_windsurf_account_id(account: &AccountDescriptor, identity: Option<&str>) -> String {
    if account.id == "windsurf:default" {
        if let Some(id_str) = identity.map(str::trim).filter(|s| !s.is_empty()) {
            return format!("windsurf:{}", id_str.to_ascii_lowercase());
        }
    }
    account.id.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{EndpointOverrides, HttpClient};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_windsurf_proc_cmdline_distinguishes_windsurf_from_antigravity() {
        let ws_cmd = b"/usr/share/windsurf/resources/app/extensions/windsurf/bin/language_server_linux_x64\x00--csrf_token\x00ws-csrf-secret-1\x00--extension_server_port\x0042100\x00--app_data_dir\x00/home/dev/.codeium/windsurf\x00";
        let parsed = parse_windsurf_proc_cmdline(ws_cmd).expect("match windsurf language_server");
        assert_eq!(parsed.csrf_token, "ws-csrf-secret-1");
        assert_eq!(parsed.extension_server_port, Some(42100));

        // Antigravity language_server MUST NOT match Windsurf!
        let agy_cmd = b"/opt/Antigravity/language_server_linux_x64\0--csrf_token\0agy-csrf-9\0--app_data_dir\0/home/dev/.gemini/antigravity\0";
        assert!(parse_windsurf_proc_cmdline(agy_cmd).is_none());
    }

    #[test]
    fn test_windsurf_mock_proc_connect_rpc_and_readonly_sqlite_fallback() {
        let tmp = std::env::temp_dir().join(format!(
            "gas-windsurf-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = tmp.join("home");
        let xdg = home.join(".config");
        let proc_root = tmp.join("proc");
        fs::create_dir_all(&xdg).unwrap();

        let provider = WindsurfProvider::new();
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 16, 0, 0).unwrap();

        // 1. Uninstalled Windsurf -> omitted cleanly (`discover_accounts` == `[]`)
        let empty_ctx = FetchContext {
            http: HttpClient::new(Duration::from_secs(3)),
            timeout: Duration::from_secs(3),
            now,
            home_dir: Some(home.clone()),
            xdg_config_dir: Some(xdg.clone()),
            proc_root: proc_root.clone(),
            endpoints: EndpointOverrides::default(),
            env_overrides: Some(BTreeMap::new()),
        };
        assert!(provider.discover_accounts(&[], true, &empty_ctx).is_empty());

        // 2. Create real SQLite `state.vscdb` containing `windsurfAuthStatus` with a secret `apiKey`
        let vscdb_path = xdg.join("Windsurf/User/globalStorage/state.vscdb");
        fs::create_dir_all(vscdb_path.parent().unwrap()).unwrap();
        let ws_api_secret = format!("{}01-superSecretWindsurfKey999999", concat!("sk-", "ws-"));
        {
            let conn = Connection::open(&vscdb_path).unwrap();
            conn.execute(
                "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)",
                [],
            )
            .unwrap();
            let auth_json = format!(
                r#"{{"name":"Cascade Dev","email":"cascade@windsurf.com","apiKey":"{ws_api_secret}","planName":"Pro"}}"#
            );
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params!["windsurfAuthStatus", auth_json],
            )
            .unwrap();
        }

        // Without running language_server -> read-only SQLite fallback yields PassiveCached & zero secret leak!
        let discovered_offline = provider.discover_accounts(&[], true, &empty_ctx);
        assert_eq!(discovered_offline.len(), 1);
        let offline_outcome = provider.fetch_account(&discovered_offline[0], None, &empty_ctx);
        match offline_outcome {
            AccountFetchOutcome::Passive(rep) => {
                assert_eq!(rep.id, "windsurf:cascade@windsurf.com");
                assert_eq!(rep.label, "cascade@windsurf.com");
                assert_eq!(rep.plan_tier.as_deref(), Some("Pro"));
                assert_eq!(rep.state, AccountState::PassiveCached);
                let dumped = serde_json::to_string(&rep).unwrap();
                assert!(!dumped.contains(&ws_api_secret));
            }
            other => panic!("expected Passive SQLite fallback, got {other:?}"),
        }

        // 3. Spin up mock Windsurf Connect-RPC server + mock `/proc/<pid>` -> Tier 1 active prompt/flow buckets!
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let rpc_handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(req.contains("x-codeium-csrf-token: ws-proc-csrf-777"));
            let body = r#"{
                "userStatus": {
                    "name": "Cascade Dev",
                    "email": "cascade@windsurf.com",
                    "planStatus": {
                        "planInfo": {
                            "planName": "Pro",
                            "teamsTier": "TEAMS_TIER_PRO",
                            "monthlyPromptCredits": 50000,
                            "monthlyFlowCredits": 150000
                        },
                        "planEnd": "2026-12-01T00:00:00Z",
                        "availablePromptCredits": 35000,
                        "availableFlowCredits": 90000,
                        "availableFlexCredits": 10000
                    }
                }
            }"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
        });

        let pid = std::process::id();
        let pid_dir = proc_root.join(pid.to_string());
        fs::create_dir_all(pid_dir.join("fd")).unwrap();
        fs::create_dir_all(proc_root.join("net")).unwrap();
        fs::write(
            pid_dir.join("cmdline"),
            b"/usr/share/windsurf/language_server_linux_x64\0--csrf_token\0ws-proc-csrf-777\0--app_data_dir\0/home/dev/.codeium/windsurf\0",
        )
        .unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("socket:[888111]", pid_dir.join("fd/4")).unwrap();
        }
        fs::write(
            proc_root.join("net/tcp"),
            format!(
                "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:{port:04X} 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 888111 1 0000000000000000 100 0 0 10 0\n"
            ),
        )
        .unwrap();

        let live_outcome = provider.fetch_account(&discovered_offline[0], None, &empty_ctx);
        rpc_handle.join().unwrap();

        match live_outcome {
            AccountFetchOutcome::Success(rep) => {
                assert_eq!(rep.id, "windsurf:cascade@windsurf.com");
                assert_eq!(rep.plan_tier.as_deref(), Some("Pro"));
                assert_eq!(rep.buckets.len(), 2);
                assert_eq!(rep.buckets[0].id, "prompt-credits");
                assert_eq!(rep.buckets[0].used_percent, 30);
                assert_eq!(rep.buckets[0].window_seconds, Some(2_678_400));
                assert_eq!(rep.buckets[1].id, "flow-credits");
                assert_eq!(rep.buckets[1].used_percent, 40);
                assert!(rep
                    .extra_info
                    .as_deref()
                    .unwrap()
                    .contains("150 / 500 prompt credits used"));
            }
            other => panic!("expected live Connect-RPC Success, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&tmp);
    }
}
