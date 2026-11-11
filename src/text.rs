use chrono::{DateTime, Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{
    AccountReport, AccountState, AgentStatusResult, CreditSummary, OverallStatus, ProviderReport,
    StatusReport, UsageBucket,
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

/// Compact JSON payload emitted by `--waybar` for Waybar `return-type: "json"` custom modules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaybarOutput {
    pub text: String,
    pub tooltip: String,
    pub class: String,
    pub percentage: u8,
}

/// Formats `report` as single-line compact Waybar custom-module JSON
/// (`{"text":"...","tooltip":"...","class":"normal|warning|critical|error","percentage":<u8>}`).
pub fn format_waybar(report: &StatusReport) -> anyhow::Result<String> {
    format_waybar_in(report, Utc::now(), &Local)
}

fn format_waybar_in<T: TimeZone>(
    report: &StatusReport,
    now: DateTime<Utc>,
    tz: &T,
) -> anyhow::Result<String>
where
    T::Offset: std::fmt::Display,
{
    let class = waybar_class(report.status).to_string();
    let percentage = report.summary.max_used_percent.min(100);
    let text = build_waybar_text(report);
    let tooltip = build_waybar_tooltip(report, now, tz);

    let payload = WaybarOutput {
        text,
        tooltip,
        class,
        percentage,
    };
    Ok(serde_json::to_string(&payload)?)
}

fn waybar_class(status: OverallStatus) -> &'static str {
    match status {
        OverallStatus::Ok => "normal",
        OverallStatus::Warning | OverallStatus::Degraded => "warning",
        OverallStatus::Critical => "critical",
        OverallStatus::Error => "error",
    }
}

fn provider_short_tag(provider_id: &str, display_name: &str, single_provider: bool) -> String {
    if single_provider {
        return collapse_line(display_name);
    }
    match provider_id {
        "claude" => "CC".to_string(),
        "codex" => "CX".to_string(),
        "grok" => "GK".to_string(),
        "cursor" => "CU".to_string(),
        "agy" => "AG".to_string(),
        "openrouter" => "OR".to_string(),
        "copilot" => "CP".to_string(),
        "gemini" => "GM".to_string(),
        "windsurf" => "WS".to_string(),
        "deepseek" => "DS".to_string(),
        _ => {
            let clean = collapse_line(display_name);
            clean
                .chars()
                .filter(|c| c.is_alphanumeric())
                .take(2)
                .collect::<String>()
                .to_uppercase()
        }
    }
}

fn build_waybar_text(report: &StatusReport) -> String {
    if !report.providers.is_empty() {
        let single = report.providers.len() == 1;
        let pills: Vec<String> = report
            .providers
            .iter()
            .map(|p| {
                let tag = provider_short_tag(&p.id, &p.display_name, single);
                let active = p.active_account();
                match p.primary_used_percent {
                    Some(pct) => format!("{tag} {pct}%"),
                    None => match active.map(|a| a.state) {
                        Some(AccountState::SignedOut | AccountState::Error) => format!("{tag} !"),
                        _ => format!("{tag} --"),
                    },
                }
            })
            .collect();
        return collapse_line(&pills.join(" · "));
    }

    if !report.agents.is_empty() {
        if report.agents.len() == 1 && report.agents[0].agent_type == "config" {
            return "AI ERR".to_string();
        }
        let pills: Vec<String> = report
            .agents
            .iter()
            .map(|a| {
                let name = collapse_line(&a.name);
                match a.used_percent {
                    Some(pct) => format!("{name} {pct}%"),
                    None if a.status == "error" || a.error.is_some() => format!("{name} !"),
                    None => format!("{name} --"),
                }
            })
            .collect();
        return collapse_line(&pills.join(" · "));
    }

    "AI --".to_string()
}

fn build_waybar_tooltip<T: TimeZone>(report: &StatusReport, now: DateTime<Utc>, tz: &T) -> String
where
    T::Offset: std::fmt::Display,
{
    let mut tooltip = format_text_in(report, false, now, tz);
    let mut extras: Vec<String> = Vec::new();
    for prov in &report.providers {
        if let Some(acct) = prov.active_account() {
            let note = acct.extra_info.as_deref().or_else(|| {
                acct.buckets
                    .iter()
                    .find_map(|b| b.extra_info.as_deref())
            });
            if let Some(info) = note.map(collapse_line).filter(|s| !s.is_empty()) {
                extras.push(format!("{}: {info}", collapse_line(&prov.display_name)));
            }
        }
    }
    if !extras.is_empty() {
        tooltip.push_str("\n\n");
        tooltip.push_str(&extras.join("\n"));
    }
    tooltip
}

/// Formats `report` as a single-line `#[fg=...]` status string suitable for `tmux` `status-right`.
pub fn format_tmux(report: &StatusReport) -> String {
    if !report.providers.is_empty() {
        let single = report.providers.len() == 1;
        let segments: Vec<String> = report
            .providers
            .iter()
            .map(|p| {
                let raw_tag = provider_short_tag(&p.id, &p.display_name, single);
                let tag = escape_tmux_text(&raw_tag);
                let active = p.active_account();
                let is_hard_error = active.is_some_and(|a| {
                    matches!(a.state, AccountState::SignedOut | AccountState::Error)
                        || a.error.is_some()
                });
                let is_stale_or_rl = active.is_some_and(|a| {
                    a.is_stale || matches!(a.state, AccountState::RateLimited)
                });

                match p.primary_used_percent {
                    Some(pct) if pct >= 90 => format!("#[fg=red,bold]{tag} {pct}%#[default]"),
                    Some(pct) if pct >= 80 || is_stale_or_rl => {
                        format!("#[fg=yellow]{tag} {pct}%#[default]")
                    }
                    Some(pct) => format!("#[fg=green]{tag} {pct}%#[default]"),
                    None if is_hard_error => format!("#[fg=red,bold]{tag} !#[default]"),
                    None => format!("#[fg=brightblack]{tag} --#[default]"),
                }
            })
            .collect();
        return segments.join(" #[fg=brightblack]|#[default] ");
    }

    if !report.agents.is_empty() {
        if report.agents.len() == 1 && report.agents[0].agent_type == "config" {
            return "#[fg=red,bold]AI ERR#[default]".to_string();
        }
        let segments: Vec<String> = report
            .agents
            .iter()
            .map(|a| {
                let tag = escape_tmux_text(&collapse_line(&a.name));
                let is_err = a.status == "error" || a.error.is_some();
                match a.used_percent {
                    Some(pct) if pct >= 90 => format!("#[fg=red,bold]{tag} {pct}%#[default]"),
                    Some(pct) if pct >= 80 || a.warning.is_some() => {
                        format!("#[fg=yellow]{tag} {pct}%#[default]")
                    }
                    Some(pct) => format!("#[fg=green]{tag} {pct}%#[default]"),
                    None if is_err => format!("#[fg=red,bold]{tag} !#[default]"),
                    None => format!("#[fg=brightblack]{tag} --#[default]"),
                }
            })
            .collect();
        return segments.join(" #[fg=brightblack]|#[default] ");
    }

    "#[fg=brightblack]AI --#[default]".to_string()
}

/// Escapes `#` as `##` and strips newlines so user-defined labels cannot inject `tmux` `#(...)` formats.
fn escape_tmux_text(input: &str) -> String {
    collapse_line(input).replace('#', "##")
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
            let sym = if cred_label.contains("(CNY)") { "¥" } else { "$" };
            parts.push(format!("{cred_label} {sym}{balance:.2}"));
        } else if let Some(used) = cred.used_dollars {
            parts.push(format!("{cred_label} ${used:.2} spent"));
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
            extra_info: None,
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
            sparkline: Vec::new(),
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
            extra_info: None,
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
            sparkline: Vec::new(),
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
            extra_info: None,
            error: None,
            login_command: Some("grok login".to_string()),
            updated_at_epoch: Some(1_794_356_800),
            buckets: Vec::new(),
            credits: None,
            sparkline: Vec::new(),
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

    #[test]
    fn test_waybar_json_classes_tooltip_and_single_line_contract() {
        let now = Utc.with_ymd_and_hms(2026, 11, 6, 16, 0, 0).unwrap();
        let pst = FixedOffset::west_opt(8 * 3600).unwrap();

        // Empty report -> `normal`, `0%`, `"AI --"`
        let empty = sample_report(vec![]);
        let empty_json = format_waybar_in(&empty, now, &pst).unwrap();
        assert!(!empty_json.contains('\n'));
        let parsed_empty: WaybarOutput = serde_json::from_str(&empty_json).unwrap();
        assert_eq!(parsed_empty.text, "AI --");
        assert_eq!(parsed_empty.class, "normal");
        assert_eq!(parsed_empty.percentage, 0);
        assert_eq!(parsed_empty.tooltip, "No agents configured");

        // Multi-provider report with Copilot `extra_info` & `89%` warning
        let copilot_bucket = UsageBucket::new(
            "premium",
            "Premium Requests",
            "31d",
            BucketLayout::FullRow,
            true,
            14.0,
            Some((1_796_083_200, "2026-12-01T00:00:00Z".to_string())),
        )
        .with_window_seconds(Some(2_678_400))
        .with_extra_info("42 / 300 premium requests used");

        let copilot_acct = AccountReport {
            id: "copilot:octodev".to_string(),
            label: "octodev".to_string(),
            source_label: Some("~/.config/github-copilot/hosts.json".to_string()),
            is_system_default: true,
            is_active: true,
            plan_tier: Some("Pro".to_string()),
            state: AccountState::ActiveOk,
            is_stale: false,
            status_note: None,
            extra_info: Some("42 / 300 premium requests used".to_string()),
            error: None,
            login_command: Some("gh auth login".to_string()),
            updated_at_epoch: Some(1_794_356_800),
            buckets: vec![copilot_bucket],
            credits: None,
            sparkline: Vec::new(),
        };

        let claude_acct = AccountReport {
            id: "claude:default".to_string(),
            label: "dev@claude.ai".to_string(),
            source_label: Some("~/.claude".to_string()),
            is_system_default: true,
            is_active: true,
            plan_tier: Some("Max".to_string()),
            state: AccountState::ActiveOk,
            is_stale: false,
            status_note: None,
            extra_info: None,
            error: None,
            login_command: Some("claude auth login".to_string()),
            updated_at_epoch: Some(1_794_356_800),
            buckets: vec![UsageBucket::new(
                "wk",
                "Weekly",
                "wk",
                BucketLayout::InlinePair,
                true,
                89.0,
                None,
            )],
            credits: None,
            sparkline: Vec::new(),
        };

        let mut report = StatusReport::from_providers(
            vec![
                ProviderReport::from_accounts(
                    "claude",
                    "Claude",
                    "provider-claude",
                    None,
                    vec![claude_acct],
                    None,
                ),
                ProviderReport::from_accounts(
                    "copilot",
                    "GitHub Copilot",
                    "provider-copilot",
                    None,
                    vec![copilot_acct],
                    None,
                ),
            ],
            " | ",
            1_794_356_800,
            None,
        );

        let wb_warn = format_waybar_in(&report, now, &pst).unwrap();
        assert!(!wb_warn.contains('\n'), "Waybar stdout payload must be single-line JSON");
        let parsed_warn: WaybarOutput = serde_json::from_str(&wb_warn).unwrap();
        assert_eq!(parsed_warn.text, "CC 89% · CP 14%");
        assert_eq!(parsed_warn.class, "warning");
        assert_eq!(parsed_warn.percentage, 89);
        assert!(parsed_warn.tooltip.contains("Claude [Max]"));
        assert!(parsed_warn.tooltip.contains("GitHub Copilot [Pro]"));
        assert!(parsed_warn
            .tooltip
            .contains("GitHub Copilot: 42 / 300 premium requests used"));

        // Critical & Error classes
        report.status = OverallStatus::Critical;
        report.summary.max_used_percent = 96;
        let parsed_crit: WaybarOutput =
            serde_json::from_str(&format_waybar_in(&report, now, &pst).unwrap()).unwrap();
        assert_eq!(parsed_crit.class, "critical");
        assert_eq!(parsed_crit.percentage, 96);

        let cfg_err = StatusReport::config_error("Invalid TOML".to_string(), 1_794_356_800);
        let parsed_err: WaybarOutput =
            serde_json::from_str(&format_waybar_in(&cfg_err, now, &pst).unwrap()).unwrap();
        assert_eq!(parsed_err.class, "error");
        assert_eq!(parsed_err.text, "AI ERR");
    }

    #[test]
    fn test_tmux_status_colors_single_line_and_format_injection_escape() {
        let empty = sample_report(vec![]);
        assert_eq!(format_tmux(&empty), "#[fg=brightblack]AI --#[default]");

        let cfg_err = StatusReport::config_error("Bad config".to_string(), 0);
        assert_eq!(format_tmux(&cfg_err), "#[fg=red,bold]AI ERR#[default]");

        // Multi-provider severity colors: <80 green, 80..89 yellow, >=90 red,bold, SignedOut red,bold !
        let mk_prov = |id: &str, name: &str, pct: Option<f64>, state: AccountState| {
            let buckets = pct
                .map(|p| {
                    vec![UsageBucket::new(
                        "wk",
                        "Weekly",
                        "wk",
                        BucketLayout::InlinePair,
                        true,
                        p,
                        None,
                    )]
                })
                .unwrap_or_default();
            ProviderReport::from_accounts(
                id,
                name,
                "icon",
                None,
                vec![AccountReport {
                    id: format!("{id}:default"),
                    label: name.to_string(),
                    source_label: None,
                    is_system_default: true,
                    is_active: true,
                    plan_tier: None,
                    state,
                    is_stale: false,
                    status_note: None,
                    extra_info: None,
                    error: None,
                    login_command: None,
                    updated_at_epoch: None,
                    buckets,
                    credits: None,
                    sparkline: Vec::new(),
                }],
                None,
            )
        };

        let multi = StatusReport::from_providers(
            vec![
                mk_prov("claude", "Claude", Some(24.0), AccountState::ActiveOk),
                mk_prov("copilot", "GitHub Copilot", Some(85.0), AccountState::ActiveOk),
                mk_prov("gemini", "Gemini CLI", Some(93.0), AccountState::ActiveOk),
                mk_prov("grok", "Grok", None, AccountState::SignedOut),
            ],
            " | ",
            0,
            None,
        );
        let tmux_out = format_tmux(&multi);
        assert!(!tmux_out.contains('\n'));
        assert_eq!(
            tmux_out,
            "#[fg=green]CC 24%#[default] #[fg=brightblack]|#[default] #[fg=yellow]CP 85%#[default] #[fg=brightblack]|#[default] #[fg=red,bold]GM 93%#[default] #[fg=brightblack]|#[default] #[fg=red,bold]GK !#[default]"
        );

        // Single-provider display_name containing `#(whoami)` must escape `#` -> `##`
        let injection = StatusReport::from_providers(
            vec![mk_prov(
                "claude",
                "Claude #(whoami)",
                Some(12.0),
                AccountState::ActiveOk,
            )],
            " | ",
            0,
            None,
        );
        let escaped = format_tmux(&injection);
        assert_eq!(escaped, "#[fg=green]Claude ##(whoami) 12%#[default]");
    }

    #[test]
    fn test_all_10_canonical_provider_short_codes_in_waybar_and_tmux() {
        let now = Utc.with_ymd_and_hms(2026, 11, 6, 16, 0, 0).unwrap();
        let pst = FixedOffset::west_opt(8 * 3600).unwrap();

        let providers: Vec<ProviderReport> = crate::model::CANONICAL_PROVIDER_IDS
            .iter()
            .enumerate()
            .map(|(idx, &id)| {
                let (display_name, icon, url, _) =
                    crate::model::canonical_provider_info(id).expect("canonical provider");
                ProviderReport::from_accounts(
                    id,
                    display_name,
                    icon,
                    Some(url.to_string()),
                    vec![AccountReport {
                        id: format!("{id}:default"),
                        label: display_name.to_string(),
                        source_label: None,
                        is_system_default: true,
                        is_active: true,
                        plan_tier: None,
                        state: AccountState::ActiveOk,
                        is_stale: false,
                        status_note: None,
                        extra_info: None,
                        error: None,
                        login_command: None,
                        updated_at_epoch: Some(1_794_356_800),
                        buckets: vec![UsageBucket::new(
                            "wk",
                            "Weekly",
                            "wk",
                            BucketLayout::InlinePair,
                            true,
                            ((idx + 1) * 5) as f64,
                            None,
                        )],
                        credits: None,
                        sparkline: Vec::new(),
                    }],
                    None,
                )
            })
            .collect();

        let report = StatusReport::from_providers(providers, " | ", 1_794_356_800, None);
        let wb: WaybarOutput =
            serde_json::from_str(&format_waybar_in(&report, now, &pst).unwrap()).unwrap();
        assert_eq!(
            wb.text,
            "CC 5% · CX 10% · GK 15% · CU 20% · AG 25% · OR 30% · CP 35% · GM 40% · WS 45% · DS 50%"
        );

        let tmux_out = format_tmux(&report);
        for code in ["CC", "CX", "GK", "CU", "AG", "OR", "CP", "GM", "WS", "DS"] {
            assert!(tmux_out.contains(&format!("{code} ")), "missing {code} in {tmux_out}");
        }
    }
}
