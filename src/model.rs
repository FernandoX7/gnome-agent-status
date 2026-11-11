//! Normalized v2 multi-provider, multi-account, multi-bucket domain model.
//!
//! Preserves 100% backwards-compatible top-level fields (`label`, `status`,
//! `updated_at_epoch`, and `agents`) while providing the hierarchical
//! `providers -> accounts -> buckets` schema for the GNOME Shell UI and CLI.

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u8 = 2;

/// Canonical provider order shown in the UI and default collector output.
pub const CANONICAL_PROVIDER_IDS: &[&str] = &["claude", "codex", "grok", "agy", "cursor"];

/// Overall health and severity state across active providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverallStatus {
    Ok,
    Warning,
    Critical,
    Degraded,
    Error,
}

impl OverallStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Critical => "critical",
            Self::Degraded => "degraded",
            Self::Error => "error",
        }
    }
}

/// Authentication and polling state for an individual provider account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    ActiveOk,
    ExpiringSoon,
    PassiveCached,
    RateLimited,
    SignedOut,
    Error,
}

impl AccountState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ActiveOk => "active_ok",
            Self::ExpiringSoon => "expiring_soon",
            Self::PassiveCached => "passive_cached",
            Self::RateLimited => "rate_limited",
            Self::SignedOut => "signed_out",
            Self::Error => "error",
        }
    }

    pub fn is_auth_error(&self) -> bool {
        matches!(self, Self::SignedOut)
    }

    pub fn is_usable(&self, has_buckets: bool) -> bool {
        match self {
            Self::ActiveOk | Self::ExpiringSoon => true,
            Self::PassiveCached => true,
            Self::RateLimited => has_buckets,
            Self::SignedOut | Self::Error => false,
        }
    }
}

/// Layout hint telling the UI whether to pair buckets 2-up inline or stack full-width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BucketLayout {
    InlinePair,
    FullRow,
}

/// Normalized quota bucket (`% used` in `0..=100`, with `remaining_percent = 100 - used_percent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageBucket {
    pub id: String,
    pub label: String,
    pub short_label: String,
    pub layout_hint: BucketLayout,
    pub is_primary: bool,
    pub used_percent: u8,
    pub remaining_percent: u8,
    pub resets_at_epoch: Option<i64>,
    pub resets_at_iso: Option<String>,
}

impl UsageBucket {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        short_label: impl Into<String>,
        layout_hint: BucketLayout,
        is_primary: bool,
        used_percent_f64: f64,
        reset: Option<(i64, String)>,
    ) -> Self {
        let clamped = if used_percent_f64.is_finite() {
            used_percent_f64.clamp(0.0, 100.0)
        } else {
            0.0
        };
        let used_percent = clamped.round() as u8;
        let remaining_percent = 100u8.saturating_sub(used_percent);
        let (resets_at_epoch, resets_at_iso) = match reset {
            Some((epoch, iso)) => (Some(epoch), Some(iso)),
            None => (None, None),
        };
        Self {
            id: id.into(),
            label: label.into(),
            short_label: short_label.into(),
            layout_hint,
            is_primary,
            used_percent,
            remaining_percent,
            resets_at_epoch,
            resets_at_iso,
        }
    }

    /// Constructs a bucket from a remaining fraction (`0.0..=1.0`), normalizing to `% used`.
    pub fn from_remaining_fraction(
        id: impl Into<String>,
        label: impl Into<String>,
        short_label: impl Into<String>,
        layout_hint: BucketLayout,
        is_primary: bool,
        remaining_fraction: f64,
        reset: Option<(i64, String)>,
    ) -> Self {
        let clamped_rem = if remaining_fraction.is_finite() {
            remaining_fraction.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let used_pct = (1.0 - clamped_rem) * 100.0;
        Self::new(
            id,
            label,
            short_label,
            layout_hint,
            is_primary,
            used_pct,
            reset,
        )
    }
}

/// Optional extra usage or prepaid credit pool summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditSummary {
    pub label: String,
    pub used_dollars: Option<f64>,
    pub limit_dollars: Option<f64>,
    pub balance_dollars: Option<f64>,
    pub used_percent: Option<u8>,
}

/// Normalized status and usage report for a single account under a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountReport {
    pub id: String,
    pub label: String,
    pub source_label: Option<String>,
    pub is_system_default: bool,
    pub is_active: bool,
    pub plan_tier: Option<String>,
    pub state: AccountState,
    pub is_stale: bool,
    pub status_note: Option<String>,
    pub error: Option<String>,
    pub login_command: Option<String>,
    pub updated_at_epoch: Option<i64>,
    pub buckets: Vec<UsageBucket>,
    pub credits: Option<CreditSummary>,
}

impl AccountReport {
    pub fn primary_bucket(&self) -> Option<&UsageBucket> {
        self.buckets
            .iter()
            .find(|b| b.is_primary)
            .or_else(|| self.buckets.first())
    }
}

/// Hierarchical usage report for a single provider and all its discovered/configured accounts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderReport {
    pub id: String,
    pub display_name: String,
    pub icon_name: String,
    pub plan_badge: Option<String>,
    pub dashboard_url: Option<String>,
    pub active_account_id: Option<String>,
    pub primary_used_percent: Option<u8>,
    pub primary_remaining_percent: Option<u8>,
    pub primary_reset_epoch: Option<i64>,
    pub primary_reset_iso: Option<String>,
    pub accounts: Vec<AccountReport>,
}

impl ProviderReport {
    /// Builds a `ProviderReport` from accounts, selecting the active account according to:
    /// 1. Explicit `pinned_account_id` (if not `"default"` and present in `accounts`),
    /// 2. Account already marked `is_active`,
    /// 3. Usable `is_system_default` account,
    /// 4. First usable account,
    /// 5. `is_system_default` account or first account.
    pub fn from_accounts(
        id: impl Into<String>,
        display_name: impl Into<String>,
        icon_name: impl Into<String>,
        dashboard_url: Option<String>,
        mut accounts: Vec<AccountReport>,
        pinned_account_id: Option<&str>,
    ) -> Self {
        let id = id.into();
        let display_name = display_name.into();
        let icon_name = icon_name.into();

        let selected_idx = select_active_account_index(&accounts, pinned_account_id);
        for (idx, acct) in accounts.iter_mut().enumerate() {
            acct.is_active = Some(idx) == selected_idx;
        }

        let active = selected_idx.and_then(|idx| accounts.get(idx));
        let active_account_id = active.map(|a| a.id.clone());
        let plan_badge = active.and_then(|a| a.plan_tier.clone());

        let primary_bucket = active
            .filter(|a| a.state.is_usable(!a.buckets.is_empty()))
            .and_then(|a| a.primary_bucket());

        let primary_used_percent = primary_bucket.map(|b| b.used_percent);
        let primary_remaining_percent = primary_bucket.map(|b| b.remaining_percent);
        let primary_reset_epoch = primary_bucket.and_then(|b| b.resets_at_epoch);
        let primary_reset_iso = primary_bucket.and_then(|b| b.resets_at_iso.clone());

        Self {
            id,
            display_name,
            icon_name,
            plan_badge,
            dashboard_url,
            active_account_id,
            primary_used_percent,
            primary_remaining_percent,
            primary_reset_epoch,
            primary_reset_iso,
            accounts,
        }
    }

    pub fn active_account(&self) -> Option<&AccountReport> {
        self.accounts
            .iter()
            .find(|a| a.is_active)
            .or_else(|| self.accounts.first())
    }

    /// Recomputes top-level provider rollup fields from its current `accounts` slice.
    pub fn refresh_rollup(&mut self, pinned_account_id: Option<&str>) {
        let selected_idx = select_active_account_index(&self.accounts, pinned_account_id);
        for (idx, acct) in self.accounts.iter_mut().enumerate() {
            acct.is_active = Some(idx) == selected_idx;
        }
        let active = selected_idx.and_then(|idx| self.accounts.get(idx));
        self.active_account_id = active.map(|a| a.id.clone());
        self.plan_badge = active.and_then(|a| a.plan_tier.clone());

        let primary_bucket = active
            .filter(|a| a.state.is_usable(!a.buckets.is_empty()))
            .and_then(|a| a.primary_bucket());

        self.primary_used_percent = primary_bucket.map(|b| b.used_percent);
        self.primary_remaining_percent = primary_bucket.map(|b| b.remaining_percent);
        self.primary_reset_epoch = primary_bucket.and_then(|b| b.resets_at_epoch);
        self.primary_reset_iso = primary_bucket.and_then(|b| b.resets_at_iso.clone());
    }
}

fn select_active_account_index(
    accounts: &[AccountReport],
    pinned_account_id: Option<&str>,
) -> Option<usize> {
    if accounts.is_empty() {
        return None;
    }

    if let Some(pin) = pinned_account_id {
        if !pin.is_empty() && pin != "default" {
            if let Some(idx) = accounts.iter().position(|a| {
                a.id == pin
                    || a.id
                        .split_once(':')
                        .is_some_and(|(_, suffix)| suffix == pin)
            }) {
                return Some(idx);
            }
        }
    }

    if pinned_account_id.is_none() {
        if let Some(idx) = accounts.iter().position(|a| a.is_active) {
            return Some(idx);
        }
    }

    if let Some(idx) = accounts
        .iter()
        .position(|a| a.is_system_default && a.state.is_usable(!a.buckets.is_empty()))
    {
        return Some(idx);
    }

    if let Some(idx) = accounts
        .iter()
        .position(|a| a.state.is_usable(!a.buckets.is_empty()))
    {
        return Some(idx);
    }

    accounts
        .iter()
        .position(|a| a.is_system_default)
        .or(Some(0))
}

/// Top-level rollup metrics used by the minimal panel indicator.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryStats {
    pub max_used_percent: u8,
    pub active_provider_count: usize,
    pub has_auth_error: bool,
}

/// Backwards-compatible v0.1 flat agent entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatusResult {
    pub name: String,
    pub agent_type: String,
    pub status: String,
    pub used_percent: Option<u8>,
    pub remaining_percent: Option<u8>,
    pub reset_info: Option<String>,
    pub detail: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// Complete v2 status payload persisted to `state.json` and printed to `stdout`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusReport {
    pub schema_version: u8,
    pub label: String,
    pub status: OverallStatus,
    pub updated_at_epoch: i64,
    pub summary: SummaryStats,
    pub providers: Vec<ProviderReport>,
    pub agents: Vec<AgentStatusResult>,
}

impl StatusReport {
    pub fn empty(now_epoch: i64) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            label: "No agents configured".to_string(),
            status: OverallStatus::Ok,
            updated_at_epoch: now_epoch,
            summary: SummaryStats::default(),
            providers: Vec::new(),
            agents: Vec::new(),
        }
    }

    pub fn config_error(err_msg: String, now_epoch: i64) -> Self {
        let safe_err = crate::http::redact_secrets(&err_msg);
        Self {
            schema_version: SCHEMA_VERSION,
            label: "Agent Status: Config Error".to_string(),
            status: OverallStatus::Error,
            updated_at_epoch: now_epoch,
            summary: SummaryStats {
                max_used_percent: 0,
                active_provider_count: 0,
                has_auth_error: false,
            },
            providers: Vec::new(),
            agents: vec![AgentStatusResult {
                name: "Config".to_string(),
                agent_type: "config".to_string(),
                status: "error".to_string(),
                used_percent: None,
                remaining_percent: None,
                reset_info: None,
                detail: None,
                warning: None,
                error: Some(safe_err),
                duration_ms: 0,
            }],
        }
    }

    /// Constructs a `StatusReport` from hierarchical `providers` and synthesizes
    /// `summary`, `status`, `label`, and legacy `agents`.
    pub fn from_providers(
        providers: Vec<ProviderReport>,
        separator: &str,
        now_epoch: i64,
        durations_ms: Option<&std::collections::BTreeMap<String, u64>>,
    ) -> Self {
        if providers.is_empty() {
            return Self::empty(now_epoch);
        }

        let mut max_used_percent: u8 = 0;
        let mut active_provider_count: usize = 0;
        let mut has_auth_error = false;
        let mut usable_count: usize = 0;
        let mut failed_count: usize = 0;
        let mut label_parts: Vec<String> = Vec::with_capacity(providers.len());
        let mut agents: Vec<AgentStatusResult> = Vec::with_capacity(providers.len());

        for provider in &providers {
            let active = provider.active_account();
            let duration_ms = durations_ms
                .and_then(|m| m.get(&provider.id).copied())
                .unwrap_or(0);

            if let Some(acct) = active {
                if acct.state.is_auth_error() {
                    has_auth_error = true;
                }
                let usable = acct.state.is_usable(!acct.buckets.is_empty());
                if usable {
                    active_provider_count += 1;
                    usable_count += 1;
                } else {
                    failed_count += 1;
                }

                if let Some(pct) = provider.primary_used_percent {
                    if pct > max_used_percent {
                        max_used_percent = pct;
                    }
                    label_parts.push(format!("{}: {}%", provider.display_name, pct));
                } else if acct.state == AccountState::PassiveCached {
                    label_parts.push(format!("{}: --", provider.display_name));
                } else {
                    label_parts.push(format!("{}: ERR", provider.display_name));
                }

                let rem_pct = provider.primary_remaining_percent;
                let reset_info = provider.primary_reset_iso.clone();
                let detail = provider
                    .primary_used_percent
                    .zip(rem_pct)
                    .map(|(u, r)| format!("{}% used ({}% remaining)", u, r));
                let warning = if acct.state == AccountState::ExpiringSoon {
                    acct.status_note.clone()
                } else {
                    None
                };
                let error = if !usable {
                    acct.error.clone().or_else(|| acct.status_note.clone())
                } else {
                    None
                };

                agents.push(AgentStatusResult {
                    name: provider.display_name.clone(),
                    agent_type: provider.id.clone(),
                    status: if usable {
                        "ok".to_string()
                    } else {
                        "error".to_string()
                    },
                    used_percent: provider.primary_used_percent,
                    remaining_percent: rem_pct,
                    reset_info,
                    detail,
                    warning,
                    error,
                    duration_ms,
                });
            } else {
                failed_count += 1;
                label_parts.push(format!("{}: ERR", provider.display_name));
                agents.push(AgentStatusResult {
                    name: provider.display_name.clone(),
                    agent_type: provider.id.clone(),
                    status: "error".to_string(),
                    used_percent: None,
                    remaining_percent: None,
                    reset_info: None,
                    detail: None,
                    warning: None,
                    error: Some("No accounts discovered".to_string()),
                    duration_ms,
                });
            }
        }

        let status = compute_overall_status(usable_count, failed_count, max_used_percent);
        let label = label_parts.join(separator);

        Self {
            schema_version: SCHEMA_VERSION,
            label,
            status,
            updated_at_epoch: now_epoch,
            summary: SummaryStats {
                max_used_percent,
                active_provider_count,
                has_auth_error,
            },
            providers,
            agents,
        }
    }

    /// Recomputes `summary`, `status`, `label`, and `agents` in-place after mutating
    /// `providers` (e.g., passive time-decay or active account switching).
    pub fn recompute_rollups(&mut self, separator: &str, pinned: Option<&std::collections::BTreeMap<String, String>>) {
        let prev_durations: std::collections::BTreeMap<String, u64> = self
            .agents
            .iter()
            .map(|a| (a.agent_type.clone(), a.duration_ms))
            .collect();
        for provider in &mut self.providers {
            let pin = pinned.and_then(|m| m.get(&provider.id).map(String::as_str));
            provider.refresh_rollup(pin);
        }
        let rebuilt = Self::from_providers(
            std::mem::take(&mut self.providers),
            separator,
            self.updated_at_epoch,
            Some(&prev_durations),
        );
        self.label = rebuilt.label;
        self.status = rebuilt.status;
        self.summary = rebuilt.summary;
        self.providers = rebuilt.providers;
        self.agents = rebuilt.agents;
    }
}

pub fn compute_overall_status(
    usable_count: usize,
    failed_count: usize,
    max_used_percent: u8,
) -> OverallStatus {
    if usable_count == 0 && failed_count > 0 {
        OverallStatus::Error
    } else if max_used_percent >= 90 {
        OverallStatus::Critical
    } else if failed_count > 0 {
        OverallStatus::Degraded
    } else if max_used_percent >= 80 {
        OverallStatus::Warning
    } else {
        OverallStatus::Ok
    }
}

/// Returns canonical `(display_name, icon_name, dashboard_url, default_ttl_secs)` for a provider id.
pub fn canonical_provider_info(
    provider_id: &str,
) -> Option<(&'static str, &'static str, &'static str, u64)> {
    match provider_id {
        "claude" => Some((
            "Claude",
            "provider-claude",
            "https://claude.ai/settings/usage",
            300,
        )),
        "codex" => Some((
            "Codex",
            "provider-codex",
            "https://chatgpt.com/codex/settings/usage",
            300,
        )),
        "grok" => Some(("Grok", "provider-grok", "https://grok.com", 300)),
        "agy" => Some((
            "Antigravity",
            "provider-agy",
            "https://antigravity.google",
            300,
        )),
        "cursor" => Some((
            "Cursor",
            "provider-cursor",
            "https://cursor.com/dashboard",
            300,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_bucket_clamps_and_sums_to_100() {
        let b = UsageBucket::new(
            "wk",
            "Weekly",
            "wk",
            BucketLayout::InlinePair,
            true,
            88.6,
            Some((1794492000, "2026-11-12T08:00:00-06:00".to_string())),
        );
        assert_eq!(b.used_percent, 89);
        assert_eq!(b.remaining_percent, 11);
        assert_eq!(b.used_percent + b.remaining_percent, 100);

        let over = UsageBucket::new("5h", "Session", "5h", BucketLayout::InlinePair, false, 145.0, None);
        assert_eq!(over.used_percent, 100);
        assert_eq!(over.remaining_percent, 0);

        let from_rem = UsageBucket::from_remaining_fraction(
            "gemini-weekly",
            "Gemini · Weekly",
            "Gemini wk",
            BucketLayout::FullRow,
            true,
            0.8415147,
            None,
        );
        assert_eq!(from_rem.used_percent, 16);
        assert_eq!(from_rem.remaining_percent, 84);
    }

    #[test]
    fn test_status_report_v2_serialization_matches_contract() {
        let acct = AccountReport {
            id: "claude:xavier@angel.com".to_string(),
            label: "xavier@angel.com".to_string(),
            source_label: Some("~/.claude".to_string()),
            is_system_default: true,
            is_active: true,
            plan_tier: Some("Max".to_string()),
            state: AccountState::ActiveOk,
            is_stale: false,
            status_note: None,
            error: None,
            login_command: Some("claude auth login".to_string()),
            updated_at_epoch: Some(1794356800),
            buckets: vec![
                UsageBucket::new(
                    "wk",
                    "Weekly",
                    "wk",
                    BucketLayout::InlinePair,
                    true,
                    89.0,
                    Some((1794492000, "2026-11-12T08:00:00-06:00".to_string())),
                ),
                UsageBucket::new(
                    "fable",
                    "Fable",
                    "Fable",
                    BucketLayout::InlinePair,
                    false,
                    0.0,
                    Some((1794492000, "2026-11-12T08:00:00-06:00".to_string())),
                ),
            ],
            credits: Some(CreditSummary {
                label: "Extra usage".to_string(),
                used_dollars: Some(14.50),
                limit_dollars: Some(100.00),
                balance_dollars: None,
                used_percent: Some(15),
            }),
        };

        let provider = ProviderReport::from_accounts(
            "claude",
            "Claude",
            "provider-claude",
            Some("https://claude.ai/settings/usage".to_string()),
            vec![acct],
            None,
        );

        let report = StatusReport::from_providers(vec![provider], " | ", 1794356800, None);
        assert_eq!(report.schema_version, 2);
        assert_eq!(report.status, OverallStatus::Warning);
        assert_eq!(report.summary.max_used_percent, 89);
        assert_eq!(report.summary.active_provider_count, 1);
        assert!(!report.summary.has_auth_error);
        assert_eq!(report.label, "Claude: 89%");
        assert_eq!(report.agents.len(), 1);
        assert_eq!(report.agents[0].used_percent, Some(89));

        let json: serde_json::Value = serde_json::to_value(&report).expect("serialize v2 report");
        assert_eq!(json["schema_version"], 2);
        assert_eq!(json["status"], "warning");
        assert_eq!(json["providers"][0]["id"], "claude");
        assert_eq!(json["providers"][0]["icon_name"], "provider-claude");
        assert_eq!(json["providers"][0]["plan_badge"], "Max");
        assert_eq!(json["providers"][0]["primary_used_percent"], 89);
        assert_eq!(json["providers"][0]["primary_remaining_percent"], 11);
        assert_eq!(json["providers"][0]["accounts"][0]["state"], "active_ok");
        assert_eq!(
            json["providers"][0]["accounts"][0]["buckets"][0]["layout_hint"],
            "inline_pair"
        );
        assert_eq!(
            json["providers"][0]["accounts"][0]["credits"]["used_dollars"],
            14.5
        );
    }

    #[test]
    fn test_overall_status_transitions() {
        assert_eq!(compute_overall_status(2, 0, 45), OverallStatus::Ok);
        assert_eq!(compute_overall_status(2, 0, 80), OverallStatus::Warning);
        assert_eq!(compute_overall_status(2, 0, 89), OverallStatus::Warning);
        assert_eq!(compute_overall_status(2, 0, 90), OverallStatus::Critical);
        assert_eq!(compute_overall_status(1, 1, 50), OverallStatus::Degraded);
        assert_eq!(compute_overall_status(1, 1, 95), OverallStatus::Critical);
        assert_eq!(compute_overall_status(0, 2, 0), OverallStatus::Error);
    }

    #[test]
    fn test_pinned_account_selection_and_default_fallback() {
        let a1 = AccountReport {
            id: "claude:personal".to_string(),
            label: "personal@example.com".to_string(),
            source_label: Some("~/.claude".to_string()),
            is_system_default: true,
            is_active: false,
            plan_tier: Some("Pro".to_string()),
            state: AccountState::ActiveOk,
            is_stale: false,
            status_note: None,
            error: None,
            login_command: None,
            updated_at_epoch: Some(100),
            buckets: vec![UsageBucket::new(
                "wk",
                "Weekly",
                "wk",
                BucketLayout::InlinePair,
                true,
                25.0,
                None,
            )],
            credits: None,
        };
        let a2 = AccountReport {
            id: "claude:work".to_string(),
            label: "work@example.com".to_string(),
            source_label: Some("~/.claude-work".to_string()),
            is_system_default: false,
            is_active: false,
            plan_tier: Some("Max".to_string()),
            state: AccountState::ActiveOk,
            is_stale: false,
            status_note: None,
            error: None,
            login_command: None,
            updated_at_epoch: Some(100),
            buckets: vec![UsageBucket::new(
                "wk",
                "Weekly",
                "wk",
                BucketLayout::InlinePair,
                true,
                72.0,
                None,
            )],
            credits: None,
        };

        let mut p = ProviderReport::from_accounts(
            "claude",
            "Claude",
            "provider-claude",
            None,
            vec![a1, a2],
            Some("claude:work"),
        );
        assert_eq!(p.active_account_id.as_deref(), Some("claude:work"));
        assert_eq!(p.primary_used_percent, Some(72));
        assert_eq!(p.plan_badge.as_deref(), Some("Max"));

        p.refresh_rollup(Some("default"));
        assert_eq!(p.active_account_id.as_deref(), Some("claude:personal"));
        assert_eq!(p.primary_used_percent, Some(25));
        assert_eq!(p.plan_badge.as_deref(), Some("Pro"));

        let mut durs = std::collections::BTreeMap::new();
        durs.insert("claude".to_string(), 42u64);
        let mut rep = StatusReport::from_providers(vec![p], " | ", 100, Some(&durs));
        assert_eq!(rep.agents[0].duration_ms, 42);
        rep.recompute_rollups(" | ", None);
        assert_eq!(rep.agents[0].duration_ms, 42);
    }
}
