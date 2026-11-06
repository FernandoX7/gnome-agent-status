use chrono::{DateTime, Local, TimeZone, Utc};

use crate::reset;
use crate::runner::{AgentStatusResult, StatusReport};

const MAX_MESSAGE_CHARS: usize = 80;

const ANSI_RED: &str = "\x1b[31m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_RESET: &str = "\x1b[0m";

struct RowData {
    name: String,
    pct: String,
    reset: String,
    message: String,
    used_percent: Option<u8>,
    is_error: bool,
}

/// Formats a status report as aligned plain-text lines, one line per agent.
/// Reset times show in ISO 8601 in the local time zone.
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
    if report.agents.is_empty() {
        return "No agents configured".to_string();
    }

    let rows: Vec<RowData> = report
        .agents
        .iter()
        .map(|agent| build_row(agent, now, tz))
        .collect();

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
    let reset_width = rows
        .iter()
        .map(|r| r.reset.chars().count())
        .max()
        .unwrap_or(0);

    rows.iter()
        .map(|row| format_row(row, name_width, pct_width, reset_width, color))
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_row<T: TimeZone>(agent: &AgentStatusResult, now: DateTime<Utc>, tz: &T) -> RowData
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

    if !row.message.is_empty() {
        if reset_width > 0 {
            let col_reset = format!("{:<reset_width$}", row.reset);
            format!("{col_name}  {col_pct}  {col_reset}  {}", row.message)
        } else {
            format!("{col_name}  {col_pct}  {}", row.message)
        }
    } else if !row.reset.is_empty() {
        format!("{col_name}  {col_pct}  {}", row.reset)
    } else {
        format!("{col_name}  {col_pct}")
    }
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
            label: String::new(),
            status: "ok".to_string(),
            updated_at_epoch: 1_700_000_000,
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
        // Put multi-byte UTF-8 characters ('é' and '·') across the 77..80 character boundary.
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
}
