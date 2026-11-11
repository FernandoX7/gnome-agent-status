//! Pluggable `UsageProvider` trait, account deduplication, and parallel collection engine.

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod grok;

use crate::cache::{
    apply_report_time_decay, default_jitter_secs, CacheStore, CachedAccountState, CollectorState,
    PollDecision,
};
use crate::config::{home_dir, xdg_config_dir, Config, ManualAccountConfig, PollMode};
use crate::http::{redact_secrets, EndpointOverrides, HttpClient};
use crate::model::{
    AccountReport, AccountState, ProviderReport, StatusReport,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// Static metadata identifying a provider implementation.
#[derive(Debug, Clone, Copy)]
pub struct ProviderMeta {
    pub id: &'static str,
    pub display_name: &'static str,
    pub icon_name: &'static str,
    pub dashboard_url: &'static str,
    pub default_ttl_secs: u64,
}

/// Discovered or manually configured account target (read-only metadata, zero network I/O).
#[derive(Debug, Clone)]
pub struct AccountDescriptor {
    pub provider_id: String,
    pub id: String,
    pub label: String,
    /// Optional canonical identity (e.g. lowercase email, account UUID, or token fingerprint)
    /// used to deduplicate symlinked/copied switcher slots.
    pub dedup_key: Option<String>,
    pub path: Option<PathBuf>,
    pub source_label: Option<String>,
    pub binary: Option<String>,
    pub is_system_default: bool,
    pub poll_mode: PollMode,
    pub login_command: Option<String>,
    pub switch_command: Option<String>,
}

/// Shared runtime environment passed to providers during discovery and fetching.
#[derive(Clone)]
pub struct FetchContext {
    pub http: HttpClient,
    pub timeout: Duration,
    pub now: DateTime<Utc>,
    pub home_dir: Option<PathBuf>,
    pub xdg_config_dir: Option<PathBuf>,
    pub proc_root: PathBuf,
    pub endpoints: EndpointOverrides,
}

impl FetchContext {
    pub fn from_config(cfg: &Config) -> Self {
        let timeout = Duration::from_secs(cfg.timeout_seconds.max(1));
        let home = home_dir();
        let xdg = xdg_config_dir(home.as_deref());
        Self {
            http: HttpClient::new(timeout),
            timeout,
            now: Utc::now(),
            home_dir: home,
            xdg_config_dir: xdg,
            proc_root: PathBuf::from("/proc"),
            endpoints: EndpointOverrides::default(),
        }
    }

    pub fn now_epoch(&self) -> i64 {
        self.now.timestamp()
    }
}

/// Outcome of polling a single account.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum AccountFetchOutcome {
    Success(AccountReport),
    /// Silently omit an opportunistic auto-discovered account (e.g. `gh` user without Copilot).
    Omit,
    RateLimited {
        retry_after_secs: Option<u64>,
    },
    SignedOut {
        message: String,
        login_command: Option<String>,
        label: Option<String>,
        plan_tier: Option<String>,
    },
    Error {
        message: String,
    },
    ErrorWithMeta {
        message: String,
        label: Option<String>,
        plan_tier: Option<String>,
    },
}

/// Trait implemented by all usage providers (`claude`, `codex`, `grok`, `agy`, `cursor`).
pub trait UsageProvider: Send + Sync {
    fn meta(&self) -> ProviderMeta;

    /// Discovers manual + optional auto-discovered local accounts without network I/O.
    fn discover_accounts(
        &self,
        manual: &[ManualAccountConfig],
        auto_discover: bool,
        ctx: &FetchContext,
    ) -> Vec<AccountDescriptor>;

    /// Returns the background cache TTL (seconds) for `account`.
    fn effective_ttl_secs(&self, _account: &AccountDescriptor, _ctx: &FetchContext) -> u64 {
        self.meta().default_ttl_secs
    }

    /// Fetches fresh usage for a single account.
    fn fetch_account(
        &self,
        account: &AccountDescriptor,
        cached: Option<&CachedAccountState>,
        ctx: &FetchContext,
    ) -> AccountFetchOutcome;
}

/// Options controlling `collect`.
#[derive(Debug, Default, Clone)]
pub struct CollectOptions {
    pub force: bool,
    pub provider_filter: Option<String>,
}

/// Registry of built-in usage providers in canonical UI display order.
pub struct ProviderRegistry {
    providers: Vec<Box<dyn UsageProvider>>,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl ProviderRegistry {
    pub fn new(providers: Vec<Box<dyn UsageProvider>>) -> Self {
        Self { providers }
    }

    pub fn builtin() -> Self {
        Self {
            providers: vec![
                Box::new(claude::ClaudeProvider::new()),
                Box::new(codex::CodexProvider::new()),
                Box::new(grok::GrokProvider::new()),
                Box::new(antigravity::AntigravityProvider::new()),
                Box::new(cursor::CursorProvider::new()),
                Box::new(copilot::CopilotProvider::new()),
            ],
        }
    }

    pub fn providers(&self) -> &[Box<dyn UsageProvider>] {
        &self.providers
    }

    /// Collects usage across enabled providers in parallel, honoring per-account TTLs,
    /// 30s `--force` debounce, HTTP 429 exponential backoff, and passive time-decay,
    /// then atomically persists `collector-cache.json` and `state.json` (`0600`).
    pub fn collect(
        &self,
        cfg: &Config,
        cache_store: &CacheStore,
        opts: &CollectOptions,
        ctx: &FetchContext,
    ) -> Result<StatusReport> {
        let now_epoch = ctx.now_epoch();
        let initial_cache = cache_store.load_collector_cache();
        let previous_report = cache_store.load_cached_report(&cfg.separator, now_epoch);

        enum PlannedSlot<'a> {
            Immediate(Box<AccountReport>),
            Poll {
                provider: &'a dyn UsageProvider,
                descriptor: Box<AccountDescriptor>,
                cached: Option<Box<CachedAccountState>>,
            },
        }

        struct ProviderPlan<'a> {
            meta: ProviderMeta,
            passthrough_report: Option<ProviderReport>,
            slots: Vec<PlannedSlot<'a>>,
            preserve_passive_agy_cache: bool,
        }

        let mut plans: Vec<ProviderPlan<'_>> = Vec::new();

        for provider in &self.providers {
            let meta = provider.meta();
            let (enabled, manual_accounts) = cfg.effective_provider_accounts(meta.id);
            if !enabled {
                continue;
            }

            if let Some(ref filter) = opts.provider_filter {
                if !filter.trim().eq_ignore_ascii_case(meta.id) {
                    if let Some(existing) = previous_report
                        .providers
                        .iter()
                        .find(|p| p.id == meta.id)
                        .cloned()
                    {
                        plans.push(ProviderPlan {
                            meta,
                            passthrough_report: Some(existing),
                            slots: Vec::new(),
                            preserve_passive_agy_cache: false,
                        });
                    }
                    continue;
                }
            }

            let raw_descriptors =
                provider.discover_accounts(&manual_accounts, cfg.auto_discover, ctx);
            let descriptors = deduplicate_accounts(raw_descriptors);

            let mut slots = Vec::with_capacity(descriptors.len());
            for desc in descriptors {
                let cached_entry = initial_cache.accounts.get(&desc.id).cloned();

                if desc.poll_mode == PollMode::Passive {
                    let mut report = if let Some(ref c) = cached_entry {
                        c.to_account_report(false, now_epoch)
                    } else {
                        AccountReport {
                            id: desc.id.clone(),
                            label: desc.label.clone(),
                            source_label: desc.source_label.clone(),
                            is_system_default: desc.is_system_default,
                            is_active: false,
                            plan_tier: None,
                            state: AccountState::PassiveCached,
                            is_stale: false,
                            status_note: None,
                            extra_info: None,
                            error: None,
                            login_command: desc.login_command.clone(),
                            updated_at_epoch: None,
                            buckets: Vec::new(),
                            credits: None,
                        }
                    };
                    report.state = AccountState::PassiveCached;
                    if report.status_note.is_none() {
                        report.status_note =
                            Some(format!("Updates when {} runs", meta.display_name));
                    }
                    slots.push(PlannedSlot::Immediate(Box::new(report)));
                    continue;
                }

                let ttl = provider.effective_ttl_secs(&desc, ctx);
                if let Some(ref c) = cached_entry {
                    match c.poll_decision(ttl, opts.force, now_epoch) {
                        PollDecision::SkipFresh => {
                            slots.push(PlannedSlot::Immediate(Box::new(
                                c.to_account_report(false, now_epoch),
                            )));
                            continue;
                        }
                        PollDecision::SkipRateLimited { .. } => {
                            let mut rep = c.to_account_report(true, now_epoch);
                            rep.state = AccountState::RateLimited;
                            rep.is_stale = true;
                            slots.push(PlannedSlot::Immediate(Box::new(rep)));
                            continue;
                        }
                        PollDecision::PollNow => {}
                    }
                }

                slots.push(PlannedSlot::Poll {
                    provider: provider.as_ref(),
                    descriptor: Box::new(desc),
                    cached: cached_entry.map(Box::new),
                });
            }

            plans.push(ProviderPlan {
                meta,
                passthrough_report: None,
                slots,
                preserve_passive_agy_cache: meta.id == "agy",
            });
        }

        // Execute all `Poll` slots concurrently via `std::thread::scope`.
        struct PollTask<'a> {
            plan_idx: usize,
            slot_idx: usize,
            provider_id: &'static str,
            provider: &'a dyn UsageProvider,
            descriptor: AccountDescriptor,
            cached: Option<CachedAccountState>,
        }

        let mut poll_tasks: Vec<PollTask<'_>> = Vec::new();
        for (plan_idx, plan) in plans.iter().enumerate() {
            for (slot_idx, slot) in plan.slots.iter().enumerate() {
                if let PlannedSlot::Poll {
                    provider,
                    descriptor,
                    cached,
                } = slot
                {
                    poll_tasks.push(PollTask {
                        plan_idx,
                        slot_idx,
                        provider_id: plan.meta.id,
                        provider: *provider,
                        descriptor: (**descriptor).clone(),
                        cached: cached.as_deref().cloned(),
                    });
                }
            }
        }

        struct CompletedPoll {
            plan_idx: usize,
            slot_idx: usize,
            provider_id: &'static str,
            descriptor: AccountDescriptor,
            outcome: AccountFetchOutcome,
            duration_ms: u64,
        }

        let completed: Vec<CompletedPoll> = thread::scope(|scope| {
            let handles: Vec<_> = poll_tasks
                .into_iter()
                .map(|task| {
                    scope.spawn(move || {
                        let start = Instant::now();
                        let outcome = task.provider.fetch_account(
                            &task.descriptor,
                            task.cached.as_ref(),
                            ctx,
                        );
                        let duration_ms = start.elapsed().as_millis() as u64;
                        CompletedPoll {
                            plan_idx: task.plan_idx,
                            slot_idx: task.slot_idx,
                            provider_id: task.provider_id,
                            descriptor: task.descriptor,
                            outcome,
                            duration_ms,
                        }
                    })
                })
                .collect();

            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .collect()
        });

        // Merge results into CollectorState and build StatusReport under exclusive cache lock.
        let _lock = cache_store.lock()?;
        let mut collector_state: CollectorState = cache_store.load_collector_cache();
        let mut durations_ms: BTreeMap<String, u64> = BTreeMap::new();

        // Index completed polls by (plan_idx, slot_idx)
        let mut completed_map: BTreeMap<(usize, usize), CompletedPoll> = BTreeMap::new();
        for item in completed {
            let entry = durations_ms
                .entry(item.provider_id.to_string())
                .or_insert(0);
            if item.duration_ms > *entry {
                *entry = item.duration_ms;
            }
            completed_map.insert((item.plan_idx, item.slot_idx), item);
        }

        let mut provider_reports: Vec<ProviderReport> = Vec::new();

        for (plan_idx, plan) in plans.into_iter().enumerate() {
            if let Some(mut passthrough) = plan.passthrough_report {
                let pin = collector_state
                    .active_accounts
                    .get(plan.meta.id)
                    .map(String::as_str);
                passthrough.refresh_rollup(pin);
                provider_reports.push(passthrough);
                continue;
            }

            let mut account_reports: Vec<AccountReport> = Vec::new();
            let mut seen_ids: HashSet<String> = HashSet::new();

            for (slot_idx, slot) in plan.slots.into_iter().enumerate() {
                match slot {
                    PlannedSlot::Immediate(rep) => {
                        seen_ids.insert(rep.id.clone());
                        account_reports.push(*rep);
                    }
                    PlannedSlot::Poll { descriptor, .. } => {
                        if let Some(done) = completed_map.remove(&(plan_idx, slot_idx)) {
                            if let Some(resolved) = apply_poll_outcome(
                                &mut collector_state,
                                &done.descriptor,
                                done.outcome,
                                now_epoch,
                            ) {
                                seen_ids.insert(resolved.id.clone());
                                account_reports.push(resolved);
                            }
                        } else {
                            let err_rep = AccountReport {
                                id: descriptor.id.clone(),
                                label: descriptor.label.clone(),
                                source_label: descriptor.source_label.clone(),
                                is_system_default: descriptor.is_system_default,
                                is_active: false,
                                plan_tier: None,
                                state: AccountState::Error,
                                is_stale: false,
                                status_note: None,
                                extra_info: None,
                                error: Some("Collector worker panicked".to_string()),
                                login_command: descriptor.login_command.clone(),
                                updated_at_epoch: Some(now_epoch),
                                buckets: Vec::new(),
                                credits: None,
                            };
                            seen_ids.insert(err_rep.id.clone());
                            account_reports.push(err_rep);
                        }
                    }
                }
            }

            // Retain passive cached Antigravity accounts seen previously under another identity.
            if plan.preserve_passive_agy_cache && !account_reports.is_empty() {
                for cached_acct in collector_state.accounts.values() {
                    if cached_acct.provider_id == "agy" && !seen_ids.contains(&cached_acct.account_id)
                    {
                        let mut passive = cached_acct.to_account_report(false, now_epoch);
                        passive.is_system_default = false;
                        passive.state = AccountState::PassiveCached;
                        passive.status_note =
                            Some("Updates when Antigravity runs".to_string());
                        seen_ids.insert(passive.id.clone());
                        account_reports.push(passive);
                    }
                }
            }

            if account_reports.is_empty() {
                continue;
            }

            let pin = collector_state
                .active_accounts
                .get(plan.meta.id)
                .map(String::as_str);

            provider_reports.push(ProviderReport::from_accounts(
                plan.meta.id,
                plan.meta.display_name,
                plan.meta.icon_name,
                Some(plan.meta.dashboard_url.to_string()),
                account_reports,
                pin,
            ));
        }

        let mut report = StatusReport::from_providers(
            provider_reports,
            &cfg.separator,
            now_epoch,
            Some(&durations_ms),
        );
        apply_report_time_decay(
            &mut report,
            &cfg.separator,
            now_epoch,
            Some(&collector_state.active_accounts),
        );

        cache_store.save_collector_cache(&collector_state)?;
        cache_store.save_state_report(&report)?;

        Ok(report)
    }
}

fn apply_poll_outcome(
    collector_state: &mut CollectorState,
    desc: &AccountDescriptor,
    outcome: AccountFetchOutcome,
    now_epoch: i64,
) -> Option<AccountReport> {
    match outcome {
        AccountFetchOutcome::Omit => {
            collector_state.accounts.remove(&desc.id);
            None
        }
        AccountFetchOutcome::Success(mut report) => {
            if report.updated_at_epoch.is_none() {
                report.updated_at_epoch = Some(now_epoch);
            }
            if report.source_label.is_none() {
                report.source_label = desc.source_label.clone();
            }
            if report.login_command.is_none() {
                report.login_command = desc.login_command.clone();
            }
            report.status_note = report.status_note.as_deref().map(redact_secrets);
            report.extra_info = report.extra_info.as_deref().map(redact_secrets);
            report.error = report.error.as_deref().map(redact_secrets);

            // If identity changed from generic `:default` to `:email`, remove stale generic key.
            if report.id != desc.id {
                collector_state.accounts.remove(&desc.id);
            }

            let entry = collector_state
                .accounts
                .entry(report.id.clone())
                .or_insert_with(|| CachedAccountState {
                    provider_id: desc.provider_id.clone(),
                    account_id: report.id.clone(),
                    label: report.label.clone(),
                    source_label: report.source_label.clone(),
                    is_system_default: report.is_system_default,
                    plan_tier: report.plan_tier.clone(),
                    state: report.state,
                    status_note: report.status_note.clone(),
                    extra_info: report.extra_info.clone(),
                    error: None,
                    login_command: report.login_command.clone(),
                    last_fetch_attempt_epoch: Some(now_epoch),
                    last_success_epoch: Some(now_epoch),
                    retry_after_epoch: None,
                    consecutive_429s: 0,
                    buckets: Vec::new(),
                    credits: None,
                });
            entry.record_success(&report, now_epoch);
            Some(entry.to_account_report(false, now_epoch))
        }
        AccountFetchOutcome::RateLimited { retry_after_secs } => {
            let entry = collector_state
                .accounts
                .entry(desc.id.clone())
                .or_insert_with(|| CachedAccountState {
                    provider_id: desc.provider_id.clone(),
                    account_id: desc.id.clone(),
                    label: desc.label.clone(),
                    source_label: desc.source_label.clone(),
                    is_system_default: desc.is_system_default,
                    plan_tier: None,
                    state: AccountState::RateLimited,
                    status_note: None,
                    extra_info: None,
                    error: None,
                    login_command: desc.login_command.clone(),
                    last_fetch_attempt_epoch: None,
                    last_success_epoch: None,
                    retry_after_epoch: None,
                    consecutive_429s: 0,
                    buckets: Vec::new(),
                    credits: None,
                });
            let jitter = default_jitter_secs(now_epoch);
            entry.record_rate_limited(retry_after_secs, now_epoch, jitter);
            let mut rep = entry.to_account_report(true, now_epoch);
            rep.state = AccountState::RateLimited;
            rep.is_stale = true;
            Some(rep)
        }
        AccountFetchOutcome::SignedOut {
            message,
            login_command,
            label,
            plan_tier,
        } => {
            let safe_msg = redact_secrets(&message);
            let resolved_label = label.unwrap_or_else(|| desc.label.clone());
            let resolved_login = login_command.or_else(|| desc.login_command.clone());
            let entry = collector_state
                .accounts
                .entry(desc.id.clone())
                .or_insert_with(|| CachedAccountState {
                    provider_id: desc.provider_id.clone(),
                    account_id: desc.id.clone(),
                    label: resolved_label.clone(),
                    source_label: desc.source_label.clone(),
                    is_system_default: desc.is_system_default,
                    plan_tier: plan_tier.clone(),
                    state: AccountState::SignedOut,
                    status_note: None,
                    extra_info: None,
                    error: Some(safe_msg.clone()),
                    login_command: resolved_login.clone(),
                    last_fetch_attempt_epoch: Some(now_epoch),
                    last_success_epoch: None,
                    retry_after_epoch: None,
                    consecutive_429s: 0,
                    buckets: Vec::new(),
                    credits: None,
                });
            entry.label = resolved_label.clone();
            if plan_tier.is_some() {
                entry.plan_tier = plan_tier.clone();
            }
            entry.state = AccountState::SignedOut;
            entry.error = Some(safe_msg.clone());
            entry.status_note = Some("Sign in again to use this account".to_string());
            entry.extra_info = None;
            entry.login_command = resolved_login.clone();
            entry.last_fetch_attempt_epoch = Some(now_epoch);
            entry.buckets.clear();

            Some(AccountReport {
                id: desc.id.clone(),
                label: resolved_label,
                source_label: desc.source_label.clone(),
                is_system_default: desc.is_system_default,
                is_active: false,
                plan_tier,
                state: AccountState::SignedOut,
                is_stale: false,
                status_note: Some("Sign in again to use this account".to_string()),
                extra_info: None,
                error: Some(safe_msg),
                login_command: resolved_login,
                updated_at_epoch: Some(now_epoch),
                buckets: Vec::new(),
                credits: None,
            })
        }
        AccountFetchOutcome::Error { message } => {
            let safe_msg = redact_secrets(&message);
            if let Some(existing) = collector_state.accounts.get_mut(&desc.id) {
                existing.last_fetch_attempt_epoch = Some(now_epoch);
                existing.error = Some(safe_msg.clone());
            }
            Some(AccountReport {
                id: desc.id.clone(),
                label: desc.label.clone(),
                source_label: desc.source_label.clone(),
                is_system_default: desc.is_system_default,
                is_active: false,
                plan_tier: None,
                state: AccountState::Error,
                is_stale: false,
                status_note: None,
                extra_info: None,
                error: Some(safe_msg),
                login_command: desc.login_command.clone(),
                updated_at_epoch: Some(now_epoch),
                buckets: Vec::new(),
                credits: None,
            })
        }
        AccountFetchOutcome::ErrorWithMeta {
            message,
            label,
            plan_tier,
        } => {
            let safe_msg = redact_secrets(&message);
            let resolved_label = label.unwrap_or_else(|| desc.label.clone());
            if let Some(existing) = collector_state.accounts.get_mut(&desc.id) {
                existing.last_fetch_attempt_epoch = Some(now_epoch);
                existing.error = Some(safe_msg.clone());
                existing.label = resolved_label.clone();
                if plan_tier.is_some() {
                    existing.plan_tier = plan_tier.clone();
                }
            }
            Some(AccountReport {
                id: desc.id.clone(),
                label: resolved_label,
                source_label: desc.source_label.clone(),
                is_system_default: desc.is_system_default,
                is_active: false,
                plan_tier,
                state: AccountState::Error,
                is_stale: false,
                status_note: None,
                extra_info: None,
                error: Some(safe_msg),
                login_command: desc.login_command.clone(),
                updated_at_epoch: Some(now_epoch),
                buckets: Vec::new(),
                credits: None,
            })
        }
    }
}

/// Deduplicates discovered and manual accounts sharing the same canonical path or `dedup_key`
/// (e.g., symlinked/copied active switcher slots) so no account is double-polled.
pub fn deduplicate_accounts(accounts: Vec<AccountDescriptor>) -> Vec<AccountDescriptor> {
    let mut result: Vec<AccountDescriptor> = Vec::with_capacity(accounts.len());

    for candidate in accounts {
        let cand_canon = candidate.path.as_deref().map(canonicalize_lossy);

        let existing_pos = result.iter().position(|existing| {
            if existing.id == candidate.id {
                return true;
            }
            if let (Some(k1), Some(k2)) = (&existing.dedup_key, &candidate.dedup_key) {
                if !k1.is_empty() && k1.eq_ignore_ascii_case(k2) {
                    return true;
                }
            }
            if let (Some(p1), Some(ref p2)) =
                (existing.path.as_deref().map(canonicalize_lossy), &cand_canon)
            {
                if p1 == *p2 {
                    return true;
                }
            }
            false
        });

        if let Some(idx) = existing_pos {
            let existing = &mut result[idx];
            existing.is_system_default |= candidate.is_system_default;
            if candidate.poll_mode == PollMode::Active {
                existing.poll_mode = PollMode::Active;
            }
            if existing.dedup_key.is_none() {
                existing.dedup_key = candidate.dedup_key;
            }
            if existing.switch_command.is_none() {
                existing.switch_command = candidate.switch_command;
            }
            if existing.binary.is_none() {
                existing.binary = candidate.binary;
            }
            // Prefer system-default source label when merging a switcher slot with active `~/.grok`
            if candidate.is_system_default && candidate.source_label.is_some() {
                existing.source_label = candidate.source_label;
            }
        } else {
            result.push(candidate);
        }
    }

    result
}

fn canonicalize_lossy(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deduplicate_accounts_merges_copied_switcher_slot() {
        let a_manual = AccountDescriptor {
            provider_id: "grok".to_string(),
            id: "grok:work".to_string(),
            label: "work@example.com".to_string(),
            dedup_key: Some("usr_abc123".to_string()),
            path: Some(PathBuf::from("/tmp/nonexistent-grok-swap/work")),
            source_label: Some("~/.grok-swap/accounts/work".to_string()),
            binary: None,
            is_system_default: false,
            poll_mode: PollMode::Passive,
            login_command: Some("grok login".to_string()),
            switch_command: Some("gswap work".to_string()),
        };
        let a_default = AccountDescriptor {
            provider_id: "grok".to_string(),
            id: "grok:default".to_string(),
            label: "work@example.com".to_string(),
            dedup_key: Some("USR_ABC123".to_string()),
            path: Some(PathBuf::from("/tmp/nonexistent-grok")),
            source_label: Some("~/.grok".to_string()),
            binary: None,
            is_system_default: true,
            poll_mode: PollMode::Active,
            login_command: Some("grok login".to_string()),
            switch_command: None,
        };

        let deduped = deduplicate_accounts(vec![a_manual, a_default]);
        assert_eq!(deduped.len(), 1);
        assert!(deduped[0].is_system_default);
        assert_eq!(deduped[0].poll_mode, PollMode::Active);
        assert_eq!(deduped[0].source_label.as_deref(), Some("~/.grok"));
        assert_eq!(deduped[0].switch_command.as_deref(), Some("gswap work"));
    }
}
