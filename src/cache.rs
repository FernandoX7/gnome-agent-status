//! Atomic `0600` XDG cache store (`state.json` + `collector-cache.json`),
//! advisory `flock` locking, HTTP 429 exponential backoff, and passive time-decay math.

use crate::config::home_dir;
use crate::model::{
    canonical_provider_info, AccountReport, AccountState, CreditSummary, ProviderReport,
    StatusReport, UsageBucket, CANONICAL_PROVIDER_IDS, SCHEMA_VERSION,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::{
    fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    io::AsRawFd,
};

/// Minimum interval (seconds) honored even when `collect --force` is requested.
pub const MANUAL_FORCE_DEBOUNCE_SECS: i64 = 30;

/// Base backoff (seconds) on HTTP 429 when `Retry-After` is absent or `0`.
pub const BASE_429_BACKOFF_SECS: u64 = 300;

/// Maximum cap (seconds) for exponential 429 backoff before jitter.
pub const MAX_429_BACKOFF_SECS: u64 = 3600;

/// Persistent internal collector metadata stored at `$XDG_CACHE_HOME/gnome-agent-status/collector-cache.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectorState {
    #[serde(default = "default_schema_version")]
    pub schema_version: u8,

    /// Pinned active account ID per provider (`provider_id -> account_id`).
    #[serde(default)]
    pub active_accounts: BTreeMap<String, String>,

    /// Per-account fetch state, 429 backoff locks, and cached bucket snapshots.
    #[serde(default)]
    pub accounts: BTreeMap<String, CachedAccountState>,
}

fn default_schema_version() -> u8 {
    SCHEMA_VERSION
}

impl Default for CollectorState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            active_accounts: BTreeMap::new(),
            accounts: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAccountState {
    pub provider_id: String,
    pub account_id: String,
    pub label: String,
    pub source_label: Option<String>,
    pub is_system_default: bool,
    pub plan_tier: Option<String>,
    pub state: AccountState,
    pub status_note: Option<String>,
    pub error: Option<String>,
    pub login_command: Option<String>,
    pub last_fetch_attempt_epoch: Option<i64>,
    pub last_success_epoch: Option<i64>,
    pub retry_after_epoch: Option<i64>,
    pub consecutive_429s: u32,
    pub buckets: Vec<UsageBucket>,
    pub credits: Option<CreditSummary>,
}

impl CachedAccountState {
    pub fn to_account_report(&self, is_stale: bool, now_epoch: i64) -> AccountReport {
        let mut buckets = self.buckets.clone();
        apply_bucket_time_decay(&mut buckets, now_epoch);

        AccountReport {
            id: self.account_id.clone(),
            label: self.label.clone(),
            source_label: self.source_label.clone(),
            is_system_default: self.is_system_default,
            is_active: false,
            plan_tier: self.plan_tier.clone(),
            state: self.state,
            is_stale,
            status_note: self.status_note.clone(),
            error: self.error.clone(),
            login_command: self.login_command.clone(),
            updated_at_epoch: self.last_success_epoch.or(self.last_fetch_attempt_epoch),
            buckets,
            credits: self.credits.clone(),
        }
    }

    /// Evaluates whether an active account should be polled now or served from cache.
    pub fn poll_decision(&self, ttl_secs: u64, force: bool, now_epoch: i64) -> PollDecision {
        if let Some(until) = self.retry_after_epoch {
            if now_epoch < until {
                return PollDecision::SkipRateLimited {
                    retry_after_epoch: until,
                };
            }
        }

        if let Some(last_ok) = self.last_success_epoch {
            let age = now_epoch.saturating_sub(last_ok);
            if force {
                if (0..MANUAL_FORCE_DEBOUNCE_SECS).contains(&age) {
                    return PollDecision::SkipFresh;
                }
            } else if (0..ttl_secs as i64).contains(&age) {
                return PollDecision::SkipFresh;
            }
        }

        PollDecision::PollNow
    }

    /// Updates state on HTTP 429, ignoring misleading `Retry-After: 0` headers.
    pub fn record_rate_limited(
        &mut self,
        retry_after_header: Option<u64>,
        now_epoch: i64,
        jitter_secs: u64,
    ) -> i64 {
        let backoff = compute_429_backoff_secs(self.consecutive_429s, retry_after_header, jitter_secs);
        self.consecutive_429s = self.consecutive_429s.saturating_add(1);
        let until = now_epoch.saturating_add(backoff as i64);
        self.retry_after_epoch = Some(until);
        self.last_fetch_attempt_epoch = Some(now_epoch);
        self.state = AccountState::RateLimited;
        let mins = ((backoff as f64) / 60.0).ceil() as u64;
        self.status_note = Some(format!("Rate limited; retrying in ~{}m", mins.max(1)));
        until
    }

    /// Updates state on successful usage fetch, clearing any 429 backoff counters.
    pub fn record_success(&mut self, report: &AccountReport, now_epoch: i64) {
        self.label = report.label.clone();
        self.source_label = report.source_label.clone();
        self.is_system_default = report.is_system_default;
        self.plan_tier = report.plan_tier.clone();
        self.state = report.state;
        self.status_note = report.status_note.clone();
        self.error = None;
        self.login_command = report.login_command.clone();
        self.last_fetch_attempt_epoch = Some(now_epoch);
        self.last_success_epoch = Some(now_epoch);
        self.retry_after_epoch = None;
        self.consecutive_429s = 0;
        self.buckets = report.buckets.clone();
        self.credits = report.credits.clone();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollDecision {
    PollNow,
    SkipFresh,
    SkipRateLimited { retry_after_epoch: i64 },
}

/// Computes `max(retry_after_header, 300 * 2^consecutive_429s).min(3600) + jitter (0..=30s)`.
/// Defeats `retry-after: 0` edge responses by always enforcing at least `300s` on first 429.
pub fn compute_429_backoff_secs(
    consecutive_429s: u32,
    retry_after_header: Option<u64>,
    jitter_secs: u64,
) -> u64 {
    let shift = consecutive_429s.min(5);
    let exp_secs = BASE_429_BACKOFF_SECS.saturating_mul(1u64 << shift);
    let header_secs = retry_after_header.unwrap_or(0);
    let base = header_secs.max(exp_secs).min(MAX_429_BACKOFF_SECS);
    base.saturating_add(jitter_secs.min(30))
}

/// Deterministic lightweight jitter in `0..30` seconds derived from current time + PID.
pub fn default_jitter_secs(now_epoch: i64) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let mix = (now_epoch.unsigned_abs())
        .wrapping_mul(6364136223846793005)
        .wrapping_add(nanos)
        .wrapping_add(std::process::id() as u64);
    mix % 30
}

/// Applies passive time-decay math (`now_epoch >= resets_at_epoch => 0% used, 100% remaining`).
/// Returns `true` if any bucket changed.
pub fn apply_bucket_time_decay(buckets: &mut [UsageBucket], now_epoch: i64) -> bool {
    let mut changed = false;
    for bucket in buckets {
        if let Some(reset_ts) = bucket.resets_at_epoch {
            if now_epoch >= reset_ts && (bucket.used_percent != 0 || bucket.remaining_percent != 100)
            {
                bucket.used_percent = 0;
                bucket.remaining_percent = 100;
                changed = true;
            }
        }
    }
    changed
}

/// Applies passive time-decay across an entire `StatusReport` and recomputes all rollups.
pub fn apply_report_time_decay(
    report: &mut StatusReport,
    separator: &str,
    now_epoch: i64,
    pinned: Option<&BTreeMap<String, String>>,
) {
    for provider in &mut report.providers {
        for account in &mut provider.accounts {
            apply_bucket_time_decay(&mut account.buckets, now_epoch);
        }
    }
    report.recompute_rollups(separator, pinned);
}

/// Advisory exclusive file lock (`flock(LOCK_EX)`) released automatically on drop.
pub struct FileLockGuard {
    file: File,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            let _ = libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// Acquires an exclusive advisory `flock` on `lock_path` (creating it with mode `0600`).
pub fn acquire_file_lock(lock_path: &Path) -> Result<FileLockGuard> {
    if let Some(parent) = lock_path.parent() {
        ensure_dir_0700(parent)?;
    }

    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        opts.mode(0o600);
    }

    let file = opts
        .open(lock_path)
        .with_context(|| format!("Failed to open lock file {}", lock_path.display()))?;

    #[cfg(unix)]
    {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("Failed to acquire flock on {}", lock_path.display()));
        }
    }

    Ok(FileLockGuard { file })
}

pub fn ensure_dir_0700(dir: &Path) -> Result<()> {
    if dir.exists() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(dir)
            .with_context(|| format!("Failed to create directory {}", dir.display()))?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create directory {}", dir.display()))?;
    }
    Ok(())
}

/// Writes `bytes` to `target` with strict `0600` permissions via temp-file + atomic `rename()`.
pub fn atomic_write_0600(target: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = target.parent() {
        ensure_dir_0700(parent)?;
    }

    let base_name = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("cache.json");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_path = target.with_file_name(format!(".{base_name}.tmp.{}.{nanos}", std::process::id()));

    {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            opts.mode(0o600);
        }
        let mut file = opts
            .open(&tmp_path)
            .with_context(|| format!("Failed to create temp cache file {}", tmp_path.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
    }

    fs::rename(&tmp_path, target).with_context(|| {
        let _ = fs::remove_file(&tmp_path);
        format!("Failed to atomically rename cache file to {}", target.display())
    })?;

    Ok(())
}

/// Manages `$XDG_CACHE_HOME/gnome-agent-status/{state.json, collector-cache.json}`.
#[derive(Debug, Clone)]
pub struct CacheStore {
    cache_dir: PathBuf,
}

impl CacheStore {
    pub fn default_cache_dir() -> PathBuf {
        if let Ok(xdg) = env::var("XDG_CACHE_HOME") {
            if !xdg.trim().is_empty() {
                return PathBuf::from(xdg).join("gnome-agent-status");
            }
        }
        let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".cache").join("gnome-agent-status")
    }

    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    pub fn from_env() -> Self {
        Self::new(Self::default_cache_dir())
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn state_path(&self) -> PathBuf {
        self.cache_dir.join("state.json")
    }

    pub fn collector_cache_path(&self) -> PathBuf {
        self.cache_dir.join("collector-cache.json")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.cache_dir.join(".collector.lock")
    }

    pub fn lock(&self) -> Result<FileLockGuard> {
        acquire_file_lock(&self.lock_path())
    }

    pub fn load_collector_cache(&self) -> CollectorState {
        let path = self.collector_cache_path();
        let Ok(raw) = fs::read_to_string(&path) else {
            return CollectorState::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    pub fn save_collector_cache(&self, cache: &CollectorState) -> Result<()> {
        let json = serde_json::to_vec_pretty(cache)?;
        atomic_write_0600(&self.collector_cache_path(), &json)
    }

    pub fn save_state_report(&self, report: &StatusReport) -> Result<()> {
        let json = serde_json::to_vec(report)?;
        atomic_write_0600(&self.state_path(), &json)
    }

    /// Reads `state.json` (or synthesizes from `collector-cache.json`), applies passive
    /// time-decay at `now_epoch`, and returns a valid `StatusReport` v2 with zero network I/O.
    pub fn load_cached_report(&self, separator: &str, now_epoch: i64) -> StatusReport {
        let collector_cache = self.load_collector_cache();
        let state_path = self.state_path();

        if let Ok(raw) = fs::read_to_string(&state_path) {
            if let Ok(mut report) = serde_json::from_str::<StatusReport>(&raw) {
                apply_report_time_decay(
                    &mut report,
                    separator,
                    now_epoch,
                    Some(&collector_cache.active_accounts),
                );
                return report;
            }
        }

        if !collector_cache.accounts.is_empty() {
            return synthesize_report_from_cache(&collector_cache, separator, now_epoch);
        }

        StatusReport::empty(now_epoch)
    }

    /// Pins `account_id` (or removes pin if `"default"`) for `provider_id`, updates both
    /// `collector-cache.json` and `state.json` under `flock`, and returns the updated report.
    pub fn select_active_account(
        &self,
        provider_id: &str,
        account_id: &str,
        separator: &str,
        now_epoch: i64,
    ) -> Result<StatusReport> {
        let _guard = self.lock()?;
        let mut collector_cache = self.load_collector_cache();
        let trimmed = account_id.trim();
        let is_reset_default = trimmed.is_empty() || trimmed == "default";
        if is_reset_default {
            collector_cache.active_accounts.remove(provider_id);
        } else {
            collector_cache
                .active_accounts
                .insert(provider_id.to_string(), trimmed.to_string());
        }
        self.save_collector_cache(&collector_cache)?;

        let mut report = self.load_cached_report(separator, now_epoch);
        if is_reset_default {
            if let Some(prov) = report.providers.iter_mut().find(|p| p.id == provider_id) {
                prov.refresh_rollup(Some("default"));
            }
        }
        report.updated_at_epoch = now_epoch;
        report.recompute_rollups(separator, Some(&collector_cache.active_accounts));
        self.save_state_report(&report)?;
        Ok(report)
    }
}

/// Synthesizes a `StatusReport` directly from `CollectorState` entries when `state.json` is absent
/// or after passive hook ingestion.
pub fn synthesize_report_from_cache(
    cache: &CollectorState,
    separator: &str,
    now_epoch: i64,
) -> StatusReport {
    let mut providers = Vec::new();
    for &provider_id in CANONICAL_PROVIDER_IDS {
        let Some((display_name, icon_name, dashboard_url, _)) =
            canonical_provider_info(provider_id)
        else {
            continue;
        };

        let accounts: Vec<AccountReport> = cache
            .accounts
            .values()
            .filter(|a| a.provider_id == provider_id)
            .map(|a| {
                let is_stale = a.state == AccountState::RateLimited;
                a.to_account_report(is_stale, now_epoch)
            })
            .collect();

        if accounts.is_empty() {
            continue;
        }

        let pin = cache.active_accounts.get(provider_id).map(String::as_str);
        providers.push(ProviderReport::from_accounts(
            provider_id,
            display_name,
            icon_name,
            Some(dashboard_url.to_string()),
            accounts,
            pin,
        ));
    }

    StatusReport::from_providers(providers, separator, now_epoch, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BucketLayout, OverallStatus};

    #[test]
    fn test_429_backoff_ignores_retry_after_zero_and_caps_at_3600() {
        // Retry-After: 0 trap MUST back off at least 300s!
        assert_eq!(compute_429_backoff_secs(0, Some(0), 0), 300);
        assert_eq!(compute_429_backoff_secs(0, None, 12), 312);
        assert_eq!(compute_429_backoff_secs(1, Some(0), 0), 600);
        assert_eq!(compute_429_backoff_secs(2, Some(0), 0), 1200);
        assert_eq!(compute_429_backoff_secs(3, Some(0), 0), 2400);
        assert_eq!(compute_429_backoff_secs(4, Some(0), 5), 3605);
        // Explicit Retry-After larger than 300 is honored up to 3600 cap.
        assert_eq!(compute_429_backoff_secs(0, Some(900), 0), 900);
        assert_eq!(compute_429_backoff_secs(0, Some(9999), 0), 3600);
    }

    #[test]
    fn test_poll_decision_ttl_force_debounce_and_429_lock() {
        let mut entry = CachedAccountState {
            provider_id: "claude".to_string(),
            account_id: "claude:default".to_string(),
            label: "dev@example.com".to_string(),
            source_label: Some("~/.claude".to_string()),
            is_system_default: true,
            plan_tier: Some("Max".to_string()),
            state: AccountState::ActiveOk,
            status_note: None,
            error: None,
            login_command: None,
            last_fetch_attempt_epoch: Some(1000),
            last_success_epoch: Some(1000),
            retry_after_epoch: None,
            consecutive_429s: 0,
            buckets: Vec::new(),
            credits: None,
        };

        // Within 300s normal TTL -> SkipFresh
        assert_eq!(entry.poll_decision(300, false, 1100), PollDecision::SkipFresh);
        // Force within 30s debounce -> SkipFresh
        assert_eq!(entry.poll_decision(300, true, 1020), PollDecision::SkipFresh);
        // Force after 30s debounce -> PollNow
        assert_eq!(entry.poll_decision(300, true, 1035), PollDecision::PollNow);
        // Normal after 300s -> PollNow
        assert_eq!(entry.poll_decision(300, false, 1301), PollDecision::PollNow);

        // Active 429 lock blocks even `--force`!
        let until = entry.record_rate_limited(Some(0), 1400, 0);
        assert_eq!(until, 1700);
        assert_eq!(
            entry.poll_decision(300, true, 1500),
            PollDecision::SkipRateLimited {
                retry_after_epoch: 1700
            }
        );
        assert_eq!(entry.poll_decision(300, false, 1700), PollDecision::PollNow);
    }

    #[test]
    fn test_cache_store_0600_permissions_time_decay_and_account_select() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "gas-cache-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = CacheStore::new(tmp_dir.clone());

        // Missing cache returns valid empty StatusReport v2 with status == "ok"
        let empty = store.load_cached_report(" | ", 1000);
        assert_eq!(empty.schema_version, 2);
        assert_eq!(empty.status, OverallStatus::Ok);
        assert!(empty.providers.is_empty());

        let acct1 = AccountReport {
            id: "claude:personal".to_string(),
            label: "personal@example.com".to_string(),
            source_label: Some("~/.claude".to_string()),
            is_system_default: true,
            is_active: true,
            plan_tier: Some("Max".to_string()),
            state: AccountState::ActiveOk,
            is_stale: false,
            status_note: None,
            error: None,
            login_command: Some("claude auth login".to_string()),
            updated_at_epoch: Some(1000),
            buckets: vec![UsageBucket::new(
                "wk",
                "Weekly",
                "wk",
                BucketLayout::InlinePair,
                true,
                95.0,
                Some((2000, "2026-11-10T00:00:00Z".to_string())),
            )],
            credits: None,
        };
        let acct2 = AccountReport {
            id: "claude:work".to_string(),
            label: "work@example.com".to_string(),
            source_label: Some("~/.claude-work".to_string()),
            is_system_default: false,
            is_active: false,
            plan_tier: Some("Pro".to_string()),
            state: AccountState::ActiveOk,
            is_stale: false,
            status_note: None,
            error: None,
            login_command: Some("CLAUDE_CONFIG_DIR=~/.claude-work claude auth login".to_string()),
            updated_at_epoch: Some(1000),
            buckets: vec![UsageBucket::new(
                "wk",
                "Weekly",
                "wk",
                BucketLayout::InlinePair,
                true,
                30.0,
                Some((5000, "2026-11-12T00:00:00Z".to_string())),
            )],
            credits: None,
        };

        let provider = ProviderReport::from_accounts(
            "claude",
            "Claude",
            "provider-claude",
            Some("https://claude.ai/settings/usage".to_string()),
            vec![acct1, acct2],
            None,
        );
        let initial = StatusReport::from_providers(vec![provider], " | ", 1000, None);
        assert_eq!(initial.status, OverallStatus::Critical);

        {
            let _lock = store.lock().expect("acquire flock");
            store.save_state_report(&initial).expect("save state.json");
            store
                .save_collector_cache(&CollectorState::default())
                .expect("save collector-cache.json");
        }

        #[cfg(unix)]
        {
            let state_mode = fs::metadata(store.state_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let meta_mode = fs::metadata(store.collector_cache_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(state_mode, 0o600);
            assert_eq!(meta_mode, 0o600);
        }

        // Before reset (t = 1500): still 95% critical
        let r_before = store.load_cached_report(" | ", 1500);
        assert_eq!(r_before.summary.max_used_percent, 95);
        assert_eq!(r_before.status, OverallStatus::Critical);

        // Switch active account to claude:work -> becomes 30% ok
        let r_switched = store
            .select_active_account("claude", "claude:work", " | ", 1600)
            .expect("select work");
        assert_eq!(
            r_switched.providers[0].active_account_id.as_deref(),
            Some("claude:work")
        );
        assert_eq!(r_switched.summary.max_used_percent, 30);
        assert_eq!(r_switched.status, OverallStatus::Ok);

        // Switch back to default, then advance past reset timestamp (t = 2500 >= 2000) -> 0% used!
        let _ = store
            .select_active_account("claude", "default", " | ", 1700)
            .expect("select default");
        let r_decayed = store.load_cached_report(" | ", 2500);
        assert_eq!(
            r_decayed.providers[0].active_account_id.as_deref(),
            Some("claude:personal")
        );
        assert_eq!(r_decayed.providers[0].primary_used_percent, Some(0));
        assert_eq!(r_decayed.providers[0].primary_remaining_percent, Some(100));
        assert_eq!(r_decayed.summary.max_used_percent, 0);
        assert_eq!(r_decayed.status, OverallStatus::Ok);
        assert_eq!(r_decayed.label, "Claude: 0%");

        let _ = fs::remove_dir_all(&tmp_dir);
    }
}
