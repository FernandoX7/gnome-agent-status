use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedUsage {
    /// Percentage of the 7-day limit used (0..=100)
    pub used_percent: u8,
    /// Percentage of the 7-day limit remaining (0..=100)
    pub remaining_percent: u8,
    /// Optional reset schedule / timestamp
    pub reset_info: Option<String>,
    /// Optional human-readable detail line
    pub detail: Option<String>,
    /// Optional warning message (e.g. Claude auth expiring soon)
    pub warning: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AgyOutput {
    #[serde(default)]
    response: Option<String>,
    #[serde(default)]
    command: Option<AgyCommand>,
}

#[derive(Debug, Deserialize)]
struct AgyCommand {
    #[serde(default)]
    data: Option<AgyUsageData>,
}

#[derive(Debug, Deserialize)]
struct AgyUsageData {
    #[serde(default)]
    groups: Vec<AgyGroup>,
}

#[derive(Debug, Deserialize)]
struct AgyGroup {
    #[serde(default)]
    name: String,
    #[serde(default)]
    buckets: Vec<AgyBucket>,
}

#[derive(Debug, Deserialize)]
struct AgyBucket {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    window: Option<String>,
    #[serde(default)]
    remaining_fraction: Option<f64>,
    #[serde(default)]
    reset_time: Option<String>,
}

/// Strips ANSI escape codes from CLI text output.
pub fn strip_ansi(input: &str) -> String {
    let ansi_re = Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").expect("valid ANSI regex");
    ansi_re.replace_all(input, "").into_owned()
}

/// Parses JSON output from `agy -p /usage --output-format json`,
/// extracting the Gemini Models 7-day (`gemini-weekly`) usage and normalizing to `% used`.
pub fn parse_agy_output(raw: &str) -> Result<ParsedUsage> {
    let trimmed = raw.trim();
    // Locate JSON object boundaries in case extra lines precede or follow the JSON payload
    let json_slice = match (trimmed.find('{'), trimmed.rfind('}')) {
        (Some(start), Some(end)) if start < end => &trimmed[start..=end],
        _ => trimmed,
    };

    let parsed: AgyOutput =
        serde_json::from_str(json_slice).context("Failed to parse JSON output from agy")?;

    // 1. Primary path: structured `command.data.groups`
    if let Some(cmd) = parsed.command {
        if let Some(data) = cmd.data {
            // Look for `id == "gemini-weekly"` first, or "Gemini Models" + "weekly"
            let bucket = data
                .groups
                .iter()
                .flat_map(|g| g.buckets.iter())
                .find(|b| b.id == "gemini-weekly")
                .or_else(|| {
                    data.groups
                        .iter()
                        .filter(|g| g.name.to_lowercase().contains("gemini"))
                        .flat_map(|g| g.buckets.iter())
                        .find(|b| {
                            b.window.as_deref() == Some("weekly")
                                || b.name.to_lowercase().contains("weekly")
                        })
                });

            if let Some(b) = bucket {
                if let Some(rem_frac) = b.remaining_fraction {
                    let clamped_rem = rem_frac.clamp(0.0, 1.0);
                    let used_percent = ((1.0 - clamped_rem) * 100.0).round() as u8;
                    let remaining_percent = (clamped_rem * 100.0).round() as u8;

                    return Ok(ParsedUsage {
                        used_percent,
                        remaining_percent,
                        reset_info: b.reset_time.clone(),
                        detail: b.description.clone().or_else(|| {
                            Some(format!(
                                "Gemini Models weekly: {}% used ({}% remaining)",
                                used_percent, remaining_percent
                            ))
                        }),
                        warning: None,
                    });
                }
            }
        }
    }

    // 2. Fallback path: parse TSV lines in `response`
    // Example: "Gemini Models\tWeekly Limit Remaining\t84%\t2026-11-07T15:56:12Z"
    if let Some(resp) = parsed.response {
        for line in resp.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 3
                && parts[0].to_lowercase().contains("gemini")
                && parts[1].to_lowercase().contains("weekly")
            {
                let pct_str = parts[2].trim().trim_end_matches('%');
                if let Ok(rem_pct) = pct_str.parse::<f64>() {
                    let clamped_rem = rem_pct.clamp(0.0, 100.0);
                    let remaining_percent = clamped_rem.round() as u8;
                    let used_percent = 100u8.saturating_sub(remaining_percent);
                    let reset_info = parts.get(3).map(|s| s.trim().to_string());

                    return Ok(ParsedUsage {
                        used_percent,
                        remaining_percent,
                        reset_info,
                        detail: Some(format!(
                            "Gemini Models weekly: {}% used ({}% remaining)",
                            used_percent, remaining_percent
                        )),
                        warning: None,
                    });
                }
            }
        }
    }

    Err(anyhow!(
        "Could not find Gemini Models weekly usage bucket in agy output"
    ))
}

/// Parses text output from `claude -p /usage`, extracting only the
/// `Current week (all models): <N>% used` line and ignoring model-specific lines like `(Fable)`.
pub fn parse_claude_output(raw: &str) -> Result<ParsedUsage> {
    let cleaned = strip_ansi(raw);

    // Capture any sign-in / auth warning lines
    let warning = cleaned
        .lines()
        .map(str::trim)
        .find(|line| line.to_lowercase().starts_with("warning:"))
        .map(String::from);

    // Match: Current week (all models): 89% used · resets Nov 10, 7am (UTC)
    let re = Regex::new(
        r"(?im)^Current\s+week\s*\(\s*all\s+models\s*\)\s*:\s*(\d+(?:\.\d+)?)\s*%\s*used(?:\s*[·•\-]\s*(.+))?$",
    )
    .expect("valid claude usage regex");

    if let Some(caps) = re.captures(&cleaned) {
        let full_line = caps.get(0).map(|m| m.as_str().trim().to_string());
        let pct_str = caps.get(1).map(|m| m.as_str()).unwrap_or("0");
        let pct_val: f64 = pct_str
            .parse()
            .context("Failed to parse percentage number from claude output")?;

        let used_percent = pct_val.clamp(0.0, 100.0).round() as u8;
        let remaining_percent = 100u8.saturating_sub(used_percent);
        let reset_info = caps.get(2).map(|m| m.as_str().trim().to_string());

        return Ok(ParsedUsage {
            used_percent,
            remaining_percent,
            reset_info,
            detail: full_line,
            warning,
        });
    }

    // Include a concise snippet of the raw output if parsing fails (e.g. auth error)
    let snippet: String = cleaned
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .join(" | ");

    if snippet.is_empty() {
        Err(anyhow!("Empty output from claude -p /usage"))
    } else {
        Err(anyhow!(
            "Could not find 'Current week (all models)' in claude output: {}",
            snippet
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_AGY_JSON: &str = r#"{"conversation_id":"","status":"SUCCESS","response":"Gemini Models\tWeekly Limit Remaining\t84%\t2026-11-07T15:56:12Z\nGemini Models\tFive Hour Limit Remaining\t93%\t2026-11-06T01:53:02Z\nClaude and GPT models\tWeekly Limit Remaining\t100%\t2026-11-12T23:14:42Z\nClaude and GPT models\tFive Hour Limit Remaining\t100%\t2026-11-06T04:14:42Z\n","duration_seconds":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0},"command":{"name":"usage","data":{"description":"Within each group, models share a weekly limit and a 5-hour limit.","groups":[{"name":"Gemini Models","description":"Models within this group: Gemini Flash, Gemini Pro","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","description":"You have used some of your weekly limit, it will fully refresh in 1 day, 16 hours.","window":"weekly","remaining_fraction":0.8415147662162781,"reset_time":"2026-11-07T15:56:12Z"},{"id":"gemini-5h","name":"Five Hour Limit Remaining","description":"You have used some of your 5-hour limit, it will fully refresh in 2 hours, 38 minutes.","window":"5h","remaining_fraction":0.9305146932601929,"reset_time":"2026-11-06T01:53:02Z"}]},{"name":"Claude and GPT models","description":"Models within this group: Claude Opus, Claude Sonnet, GPT-OSS","buckets":[{"id":"3p-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":1,"reset_time":"2026-11-12T23:14:42Z"},{"id":"3p-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":1,"reset_time":"2026-11-06T04:14:42Z"}]}]}}}"#;

    const SAMPLE_CLAUDE_TEXT: &str = r#"Warning: this sign-in ends between 2026-11-07 and 2026-11-10 (in 2 days at the earliest). Renewing it does not move those dates. Run claude auth login before then, or use claude setup-token for unattended jobs.
You are currently using your subscription to power your Claude Code usage

Current week (all models): 89% used · resets Nov 10, 7am (UTC)
Current week (Fable): 46% used · resets Nov 10, 7am (UTC)

What's contributing to your usage?
Approximate · sessions on this machine only · shares overlap
"#;

    #[test]
    fn test_parse_agy_output() {
        let usage = parse_agy_output(SAMPLE_AGY_JSON).expect("should parse agy json");
        // remaining_fraction is 0.8415 -> used is 15.85% -> rounds to 16%
        assert_eq!(usage.used_percent, 16);
        assert_eq!(usage.remaining_percent, 84);
        assert_eq!(usage.reset_info.as_deref(), Some("2026-11-07T15:56:12Z"));
        assert!(usage
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("refresh in 1 day, 16 hours"));
    }

    #[test]
    fn test_parse_claude_output_ignores_fable() {
        let usage = parse_claude_output(SAMPLE_CLAUDE_TEXT).expect("should parse claude text");
        assert_eq!(usage.used_percent, 89);
        assert_eq!(usage.remaining_percent, 11);
        assert_eq!(
            usage.reset_info.as_deref(),
            Some("resets Nov 10, 7am (UTC)")
        );
        assert!(usage
            .warning
            .as_deref()
            .unwrap_or_default()
            .starts_with("Warning: this sign-in ends"));
    }
}
