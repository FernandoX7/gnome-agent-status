use chrono::{DateTime, Local, TimeZone, Utc};

use crate::model::{
    AccountReport, AccountState, AgentStatusResult, CreditSummary, ProviderReport, StatusReport,
    UsageBucket,
};
use crate::reset;

const MAX_MESSAGE_CHARS: usize = 80;
const MAX_BUCKETS_CHARS: usize = 96;

const ANSI_RED: &str = "\x1b[31m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_RESET: &str = "\x1b[0m";

struct RowData {
    name: String,
    pct: String,
    buckets: String,
    reset: String,
    message: String,
    used_percent: Option<u8>,
    is_error: bool,
}

/// Formats a status report as aligned plain-text lines.
///
/// Uses the hierarchical v2 `providers` table (with plan badges, active multi-account labels,
/// and secondary bucket/credit summaries) when `report.providers` is populated, and falls back
/// to the legacy `agents` table when `report.providers` is empty.
pub fn format_text(report: &StatusReport, color: bool) -> String {
    format_text_in(report, color, Utc::now(), &Local)
}

/// Formats a status report with reset times in the time zone `tz`.
fn format_text_in<T: TimeZone>(
    report: &StatusReport,
    color: bool,
    now: DateTime<Utc>,
    tz: &T,
) -> String
where
    T::Offset: std::fmt::Display,
{
    if report.providers.is_empty() && report.agents.is_empty() {
        return "No agents configured".to_string();
    }

    let rows: Vec<RowData> = if !report.providers.is_empty() {
        report
            .providers
            .iter()
            .map(|provider| build_provider_row(provider, now, tz))
            .collect()
    } else {
        report
            .agents
            .iter()
            .map(|agent| build_legacy_row(agent, now, tz))
            .collect()
    };

    let name_width = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0);
    let pct_width = rows
        .iter()
        .map(|r| r.pct.chars().count())
        .max()
        .unwrap_or(0);
    let buckets_width = rows
        .iter()
        .map(|r| r.buckets.chars().count())
        .max()
        .unwrap_or(0);
    let reset_width = rows
        .iter()
        .map(|r| r.reset.chars().count())
        .max()
        .unwrap_or(0);

    rows.iter()
        .map(|row| format_row(row, name_width, pct_width, buckets_width, reset_width, color))
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_provider_row<T: TimeZone>(
    provider: &ProviderReport,
    now: DateTime<Utc>,
    tz: &T,
) -> RowData
where
    T::Offset: std::fmt::Display,
{
    let active = provider.active_account();
    let name = format_provider_title(provider, active);

    let pct = match provider.primary_used_percent {
        Some(value) => format!("{value}%"),
        None => "--".to_string(),
    };

    let buckets = active
        .map(|acct| format_account_buckets_and_credits(&acct.buckets, acct.credits.as_ref()))
        .unwrap_or_default();

    let reset = provider
        .primary_reset_epoch
        .and_then(|epoch| reset::normalize_epoch(epoch, tz).map(|(_, iso)| iso))
        .or_else(|| {
            provider
                .primary_reset_iso
                .as_deref()
                .map(collapse_line)
                .map(|r| reset::to_iso(&r, now, tz).unwrap_or(r))
        })
        .unwrap_or_default();

    let raw_message = active.and_then(|acct| {
        acct.error
            .as_deref()
            .map(collapse_line)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                acct.status_note
                    .as_deref()
                    .map(collapse_line)
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| match acct.state {
                AccountState::SignedOut => Some(match acct.login_command.as_deref() {
                    Some(cmd) if !cmd.trim().is_empty() => {
                        format!("Sign in required ({})", cmd.trim())
                    }
                    _ => "Sign in required".to_string(),
                }),
                _ => None,
            })
    });

    let message = raw_message
        .map(|s| truncate_chars(&s, MAX_MESSAGE_CHARS))
        .unwrap_or_default();

    let is_error = match active {
        Some(acct) => {
            matches!(acct.state, AccountState::SignedOut | AccountState::Error)
                || acct.error.is_some()
                || provider.primary_used_percent.is_none()
        }
        None => true,
    };

    RowData {
        name,
        pct,
        buckets,
        reset,
        message,
        used_percent: provider.primary_used_percent,
        is_error,
    }
}

fn format_provider_title(provider: &ProviderReport, active: Option<&AccountReport>) -> String {
    let mut title = collapse_line(&provider.display_name);
    if let Some(badge) = provider
        .plan_badge
        .as_deref()
        .map(collapse_line)
        .filter(|b| !b.is_empty())
    {
        title.push_str(&format!(" [{badge}]"));
    }
    if provider.accounts.len() > 1 {
        if let Some(acct) = active {
            let acct_label = collapse_line(&acct.label);
            if !acct_label.is_empty()
                && !acct_label.eq_ignore_ascii_case(&provider.display_name)
            {
                title.push_str(&format!(" ({acct_label})"));
            }
        }
    }
    title
}

fn format_account_buckets_and_credits(
    buckets: &[UsageBucket],
    credits: Option<&CreditSummary>,
) -> String {
    let mut parts: Vec<String> = buckets
        .iter()
        .map(|b| {
            let label = collapse_line(&b.short_label);
            format!("{label} {}%", b.used_percent)
        })
        .collect();

    if let Some(cred) = credits {
        let cred_label = collapse_line(&cred.label);
        if let (Some(used), Some(limit)) = (cred.used_dollars, cred.limit_dollars) {
            parts.push(format!("{cred_label} ${used:.2}/${limit:.2}"));
        } else if let Some(balance) = cred.balance_dollars {
            parts.push(format!("{cred_label} ${balance:.2}"));
        }
    }

    if parts.is_empty() {
        String::new()
    } else {
        truncate_chars(&parts.join(" · "), MAX_BUCKETS_CHARS)
    }
}

fn build_legacy_row<T: TimeZone>(agent: &AgentStatusResult, now: DateTime<Utc>, tz: &T) -> RowData
where
    T::Offset: std::fmt::Display,
{
    let name = collapse_line(&agent.name);
    let pct = match agent.used_percent {
        Some(value) => format!("{value}%"),
        None => "--".to_string(),
    };
    let reset = agent
        .reset_info
        .as_deref()
        .map(collapse_line)
        .map(|r| reset::to_iso(&r, now, tz).unwrap_or(r))
        .unwrap_or_default();

    let raw_message = agent
        .error
        .as_deref()
        .map(collapse_line)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            agent
                .warning
                .as_deref()
                .map(collapse_line)
                .filter(|s| !s.is_empty())
        });

    let message = raw_message
        .map(|s| truncate_chars(&s, MAX_MESSAGE_CHARS))
        .unwrap_or_default();

    let is_error = agent.status == "error" || agent.error.is_some() || agent.used_percent.is_none();

    RowData {
        name,
        pct,
        buckets: String::new(),
        reset,
        message,
        used_percent: agent.used_percent,
        is_error,
    }
}

fn format_row(
    row: &RowData,
    name_width: usize,
    pct_width: usize,
    buckets_width: usize,
    reset_width: usize,
    color: bool,
) -> String {
    let col_name = format!("{:<name_width$}", row.name);
    let padded_pct = format!("{:>pct_width$}", row.pct);
    let col_pct = if color {
        colorize_pct(&padded_pct, row.used_percent, row.is_error)
    } else {
        padded_pct
    };

    let mut out = format!("{col_name}  {col_pct}");

    let has_reset = !row.reset.is_empty();
    let has_msg = !row.message.is_empty();

    if buckets_width > 0 {
        if has_reset || has_msg {
            out.push_str(&format!("  {:<buckets_width$}", row.buckets));
        } else if !row.buckets.is_empty() {
            out.push_str(&format!("  {}", row.buckets));
        }
    }

    if has_msg {
        if reset_width > 0 {
            out.push_str(&format!("  {:<reset_width$}  {}", row.reset, row.message));
        } else {
            out.push_str(&format!("  {}", row.message));
        }
    } else if has_reset {
        out.push_str(&format!("  {}", row.reset));
    }

    out
}

fn colorize_pct(padded_pct: &str, used_percent: Option<u8>, is_error: bool) -> String {
    let code = if is_error {
        ANSI_RED
    } else {
        match used_percent {
            Some(pct) if pct < 50 => ANSI_GREEN,
            Some(pct) if pct < 80 => ANSI_YELLOW,
            Some(_) => ANSI_RED,
            None => ANSI_RED,
        }
    };
    format!("{code}{padded_pct}{ANSI_RESET}")
}

/// Replaces line breaks and extra whitespace with single spaces.
fn collapse_line(input: &str) -> String {
    input
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Cuts text to a maximum character count on a Unicode character boundary.
fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let keep = max_chars - 3;
    let prefix: String = input.chars().take(keep).collect();
    format!("{prefix}...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BucketLayout, OverallStatus, SummaryStats, SCHEMA_VERSION};
    use crate::parser::strip_ansi;
    use chrono::FixedOffset;

    /// Formats in a fixed zone (UTC-8), so that the result does not change with the machine zone.
    fn format_text(report: &StatusReport, color: bool) -> String {
        let now = Utc.with_ymd_and_hms(2026, 11, 6, 16, 0, 0).unwrap();
        let pst = FixedOffset::west_opt(8 * 3600).unwrap();
        format_text_in(report, color, now, &pst)
    }

    fn sample_agent(
        name: &str,
        status: &str,
        used_percent: Option<u8>,
        reset_info: Option<&str>,
        warning: Option<&str>,
        error: Option<&str>,
    ) -> AgentStatusResult {
        AgentStatusResult {
            name: name.to_string(),
            agent_type: "claude".to_string(),
            status: status.to_string(),
            used_percent,
            remaining_percent: used_percent.map(|u| 100u8.saturating_sub(u)),
            reset_info: reset_info.map(String::from),
            detail: None,
            warning: warning.map(String::from),
            error: error.map(String::from),
            duration_ms: 10,
        }
    }

    fn sample_report(agents: Vec<AgentStatusResult>) -> StatusReport {
        StatusReport {
            schema_version: SCHEMA_VERSION,
            label: String::new(),
            status: OverallStatus::Ok,
            updated_at_epoch: 1_700_000_000,
            summary: SummaryStats::default(),
            providers: Vec::new(),
            agents,
        }
    }

    #[test]
    fn test_alignment_with_different_name_lengths() {
        let report = sample_report(vec![
            sample_agent(
                "AGY",
                "ok",
                Some(5),
                Some("2026-11-07T15:56:12Z"),
                Some("Warning: token expires soon"),
                None,
            ),
            sample_agent(
                "Claude Work",
                "ok",
                Some(100),
                Some("resets Nov 10, 7am (America/Los_Angeles)"),
                None,
                None,
            ),
            sample_agent(
                "Claude",
                "error",
                None,
                None,
                None,
                Some("Command timed out after 30s"),
            ),
        ]);

        let out = format_text(&report, false);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0],
            "AGY            5%  2026-11-07T07:56:12-08:00  Warning: token expires soon"
        );
        assert_eq!(lines[1], "Claude Work  100%  2026-11-10T07:00:00-08:00");
        assert_eq!(
            lines[2],
            "Claude         --                             Command timed out after 30s"
        );

        let msg_col_0 = lines[0].find("Warning:").expect("warning column");
        let msg_col_2 = lines[2].find("Command").expect("error column");
        assert_eq!(msg_col_0, msg_col_2);
    }

    #[test]
    fn test_missing_percentage_uses_dashes() {
        let report = sample_report(vec![sample_agent(
            "Claude",
            "error",
            None,
            None,
            None,
            Some("Auth error"),
        )]);

        let out = format_text(&report, false);
        assert_eq!(out, "Claude  --  Auth error");
    }

    #[test]
    fn test_error_warning_and_reset_collapse_to_one_line() {
        let report = sample_report(vec![
            sample_agent(
                "Claude",
                "error",
                None,
                Some("resets Nov 10\n7am UTC"),
                None,
                Some("Line one\nLine two\r\nLine three"),
            ),
            sample_agent(
                "AGY",
                "ok",
                Some(20),
                Some("2026-11-07\r\n15:56:12Z"),
                Some("Warning line 1\nWarning line 2"),
                None,
            ),
        ]);

        let out = format_text(&report, false);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("resets Nov 10 7am UTC"));
        assert!(lines[0].contains("Line one Line two Line three"));
        assert!(lines[1].contains("2026-11-07T07:56:12-08:00"));
        assert!(lines[1].contains("Warning line 1 Warning line 2"));
    }

    #[test]
    fn test_cut_long_error_text_on_char_boundary() {
        let long_err = format!("{}é·é·{}", "a".repeat(75), "b".repeat(30));
        let report = sample_report(vec![sample_agent(
            "Claude",
            "error",
            None,
            None,
            None,
            Some(&long_err),
        )]);

        let out = format_text(&report, false);
        let prefix = "Claude  --  ";
        assert!(out.starts_with(prefix));
        let err_part = &out[prefix.len()..];
        assert_eq!(err_part.chars().count(), 80);
        assert!(err_part.ends_with("..."));
        assert_eq!(err_part, format!("{}é·...", "a".repeat(75)));
    }

    #[test]
    fn test_no_ansi_codes_without_color() {
        let report = sample_report(vec![
            sample_agent("Low", "ok", Some(16), Some("reset 1"), None, None),
            sample_agent("Mid", "ok", Some(65), Some("reset 2"), None, None),
            sample_agent("High", "ok", Some(89), Some("reset 3"), None, None),
            sample_agent("Err", "error", None, None, None, Some("failed")),
        ]);

        let out = format_text(&report, false);
        assert!(!out.contains("\x1b["));
    }

    #[test]
    fn test_ansi_codes_with_color_and_alignment() {
        let report = sample_report(vec![
            sample_agent("Low", "ok", Some(16), Some("reset 1"), None, None),
            sample_agent("Mid", "ok", Some(65), Some("reset 2"), None, None),
            sample_agent("High", "ok", Some(89), Some("reset 3"), None, None),
            sample_agent("Err", "error", None, None, None, Some("failed")),
        ]);

        let plain = format_text(&report, false);
        let colored = format_text(&report, true);

        assert!(colored.contains("\x1b[32m16%\x1b[0m"));
        assert!(colored.contains("\x1b[33m65%\x1b[0m"));
        assert!(colored.contains("\x1b[31m89%\x1b[0m"));
        assert!(colored.contains("\x1b[31m --\x1b[0m"));
        assert_eq!(strip_ansi(&colored), plain);
    }

    #[test]
    fn test_empty_agent_list() {
        let report = sample_report(vec![]);
        assert_eq!(format_text(&report, false), "No agents configured");
        assert_eq!(format_text(&report, true), "No agents configured");
    }

    #[test]
    fn test_v2_providers_multi_bucket_table_and_color_alignment() {
        let claude_work = AccountReport {
            id: "claude:work".to_string(),
            label: "work@company.com".to_string(),
            source_label: Some("~/.claude-work".to_string()),
            is_system_default: false,
            is_active: true,
            plan_tier: Some("Max".to_string()),
            state: AccountState::ActiveOk,
            is_stale: false,
            status_note: None,
            error: None,
            login_command: Some("claude auth login".to_string()),
            updated_at_epoch: Some(1_794_356_800),
            buckets: vec![
                UsageBucket::new(
                    "wk",
                    "Weekly",
                    "wk",
                    BucketLayout::InlinePair,
                    true,
                    89.0,
                    Some((1_794_492_000, "2026-11-12T14:00:00Z".to_string())),
                ),
                UsageBucket::new(
                    "5h",
                    "Session (5h)",
                    "5h",
                    BucketLayout::InlinePair,
                    false,
                    37.0,
                    None,
                ),
                UsageBucket::new(
                    "fable",
                    "Fable",
                    "Fable",
                    BucketLayout::InlinePair,
                    false,
                    12.0,
                    None,
                ),
            ],
            credits: Some(CreditSummary {
                label: "Extra".to_string(),
                used_dollars: Some(4.5),
                limit_dollars: Some(50.0),
                balance_dollars: Some(45.5),
                used_percent: Some(9),
            }),
        };

        let claude_personal = AccountReport {
            id: "claude:personal".to_string(),
            label: "personal@home.io".to_string(),
            source_label: Some("~/.claude".to_string()),
            is_system_default: true,
            is_active: false,
            plan_tier: Some("Pro".to_string()),
            state: AccountState::ActiveOk,
            is_stale: false,
            status_note: None,
            error: None,
            login_command: Some("claude auth login".to_string()),
            updated_at_epoch: Some(1_794_356_800),
            buckets: vec![UsageBucket::new(
                "wk",
                "Weekly",
                "wk",
                BucketLayout::InlinePair,
                true,
                20.0,
                None,
            )],
            credits: None,
        };

        let grok_signed_out = AccountReport {
            id: "grok:default".to_string(),
            label: "Grok".to_string(),
            source_label: Some("~/.grok".to_string()),
            is_system_default: true,
            is_active: true,
            plan_tier: None,
            state: AccountState::SignedOut,
            is_stale: false,
            status_note: None,
            error: None,
            login_command: Some("grok login".to_string()),
            updated_at_epoch: Some(1_794_356_800),
            buckets: Vec::new(),
            credits: None,
        };

        let providers = vec![
            ProviderReport::from_accounts(
                "claude",
                "Claude",
                "provider-claude",
                Some("https://claude.ai/settings/usage".to_string()),
                vec![claude_work, claude_personal],
                Some("claude:work"),
            ),
            ProviderReport::from_accounts(
                "grok",
                "Grok",
                "provider-grok",
                Some("https://grok.com".to_string()),
                vec![grok_signed_out],
                None,
            ),
        ];

        let report = StatusReport::from_providers(providers, " | ", 1_794_356_800, None);
        let plain = format_text(&report, false);
        let colored = format_text(&report, true);

        assert_eq!(strip_ansi(&colored), plain);
        assert!(plain.contains("Claude [Max] (work@company.com)"));
        assert!(plain.contains("wk 89% · 5h 37% · Fable 12% · Extra $4.50/$50.00"));
        assert!(plain.contains("Sign in required (grok login)"));
        assert!(colored.contains("\x1b[31m89%\x1b[0m"));
        assert!(colored.contains("\x1b[31m --\x1b[0m"));
    }
}
