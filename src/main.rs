use anyhow::{Context, Result};
use clap::Parser;
use gnome_agent_status::config::Config;
use gnome_agent_status::model::StatusReport;
use gnome_agent_status::{runner, text};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "gnome-agent-status",
    version,
    about = "Queries usage limits for configured AI coding agents in parallel and prints JSON for the GNOME Shell extension."
)]
struct Cli {
    /// Path to configuration file (TOML or JSON).
    /// Defaults to ~/.config/gnome-agent-status/config.toml
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Initialize default config file at ~/.config/gnome-agent-status/config.toml if missing
    #[arg(long)]
    init_config: bool,

    /// Pretty-print the JSON output
    #[arg(long)]
    pretty: bool,

    /// Print one status line per agent in plain text
    #[arg(long)]
    text: bool,

    /// Add ANSI colors to plain-text output
    #[arg(long)]
    color: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.init_config {
        let target = cli
            .config
            .clone()
            .unwrap_or_else(Config::default_config_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory {}", parent.display())
            })?;
        }
        if !target.exists() {
            fs::write(&target, Config::sample_toml()).with_context(|| {
                format!("Failed to write sample config to {}", target.display())
            })?;
            eprintln!("Created default config at {}", target.display());
        } else {
            eprintln!("Config already exists at {}", target.display());
        }
        return Ok(());
    }

    let cfg = match Config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(err) => {
            // Even if config fails to parse, emit valid JSON so the GNOME top bar can show the error cleanly
            let fallback = StatusReport::config_error(format!("{:#}", err), 0);
            let out = format_report(&fallback, &cli)?;
            println!("{}", out);
            return Ok(());
        }
    };

    let report = runner::collect_status(&cfg);
    let out = format_report(&report, &cli)?;
    println!("{}", out);

    Ok(())
}

fn format_report(report: &StatusReport, cli: &Cli) -> Result<String> {
    if cli.text {
        Ok(text::format_text(report, cli.color))
    } else if cli.pretty {
        Ok(serde_json::to_string_pretty(report)?)
    } else {
        Ok(serde_json::to_string(report)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gnome_agent_status::model::{AgentStatusResult, OverallStatus, SummaryStats, SCHEMA_VERSION};

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
}
