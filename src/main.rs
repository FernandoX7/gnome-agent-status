use anyhow::{Context, Result};
use clap::Parser;
use gnome_agent_status::cli::{execute_cli, Cli, CliRuntime, Commands};
use gnome_agent_status::http::redact_secrets;
use std::io::{self, Read};
use std::process::ExitCode;

fn main() -> ExitCode {
    match run_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {}", redact_secrets(&format!("{err:#}")));
            ExitCode::FAILURE
        }
    }
}

fn run_main() -> Result<()> {
    let cli = Cli::parse();

    let stdin_owned = if matches!(cli.command, Some(Commands::IngestHook { .. })) {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .context("Failed to read statusLine JSON from stdin")?;
        Some(buf)
    } else {
        None
    };

    let runtime = CliRuntime {
        stdin_data: stdin_owned.as_deref(),
        ..CliRuntime::default()
    };

    if let Some(output) = execute_cli(&cli, &runtime)? {
        println!("{output}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gnome_agent_status::cli::format_report;
    use gnome_agent_status::model::{
        AgentStatusResult, OverallStatus, StatusReport, SummaryStats, SCHEMA_VERSION,
    };

    #[test]
    fn test_cli_text_overrides_pretty() {
        let cli = Cli::parse_from(["gnome-agent-status", "--text", "--pretty", "--color"]);
        assert!(cli.text);
        assert!(cli.pretty);
        assert!(cli.color);

        let report = StatusReport {
            schema_version: SCHEMA_VERSION,
            label: "AGY: 16%".to_string(),
            status: OverallStatus::Ok,
            updated_at_epoch: 0,
            summary: SummaryStats {
                max_used_percent: 16,
                active_provider_count: 1,
                has_auth_error: false,
            },
            providers: Vec::new(),
            agents: vec![AgentStatusResult {
                name: "AGY".to_string(),
                agent_type: "agy".to_string(),
                status: "ok".to_string(),
                used_percent: Some(16),
                remaining_percent: Some(84),
                reset_info: Some("2026-11-07T15:56:12Z".to_string()),
                detail: None,
                warning: None,
                error: None,
                duration_ms: 5,
            }],
        };

        let out = format_report(&report, &cli).expect("format report");
        assert!(!out.starts_with('{'));
        assert!(out.contains("AGY"));
        assert!(out.contains("\x1b[32m16%\x1b[0m"));
    }

    #[test]
    fn test_cli_waybar_and_tmux_format_dispatch() {
        let report = StatusReport {
            schema_version: SCHEMA_VERSION,
            label: "AGY: 16%".to_string(),
            status: OverallStatus::Ok,
            updated_at_epoch: 0,
            summary: SummaryStats {
                max_used_percent: 16,
                active_provider_count: 1,
                has_auth_error: false,
            },
            providers: Vec::new(),
            agents: vec![AgentStatusResult {
                name: "AGY".to_string(),
                agent_type: "agy".to_string(),
                status: "ok".to_string(),
                used_percent: Some(16),
                remaining_percent: Some(84),
                reset_info: Some("2026-11-07T15:56:12Z".to_string()),
                detail: None,
                warning: None,
                error: None,
                duration_ms: 5,
            }],
        };

        let wb_cli = Cli::parse_from(["gnome-agent-status", "--waybar"]);
        let wb_out = format_report(&report, &wb_cli).expect("waybar format");
        assert!(!wb_out.contains('\n'));
        assert!(wb_out.contains(r#""class":"normal""#));
        assert!(wb_out.contains(r#""percentage":16"#));

        let tmux_cli = Cli::parse_from(["gnome-agent-status", "cached", "--tmux"]);
        let tmux_out = format_report(&report, &tmux_cli).expect("tmux format");
        assert_eq!(tmux_out, "#[fg=green]AGY 16%#[default]");
    }
}
