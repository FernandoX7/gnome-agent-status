use crate::config::{expand_tilde, resolve_binary, AgentConfig, AgentType, Config};
use crate::model::{
    compute_overall_status, AgentStatusResult, OverallStatus, StatusReport, SummaryStats,
    SCHEMA_VERSION,
};
use crate::parser::{parse_agy_output, parse_claude_output};
use crate::proc_exec::{run_command, CommandSpec};
use anyhow::{anyhow, Result};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Runs all configured agents in parallel and aggregates their usage into a `StatusReport`.
pub fn collect_status(config: &Config) -> StatusReport {
    let timeout = Duration::from_secs(config.timeout_seconds.max(1));

    let results: Vec<AgentStatusResult> = thread::scope(|scope| {
        let handles: Vec<_> = config
            .agents
            .iter()
            .map(|agent| scope.spawn(move || query_single_agent(agent, timeout)))
            .collect();

        handles
            .into_iter()
            .map(|h| {
                h.join().unwrap_or_else(|_| AgentStatusResult {
                    name: "Unknown".to_string(),
                    agent_type: "unknown".to_string(),
                    status: "error".to_string(),
                    used_percent: None,
                    remaining_percent: None,
                    reset_info: None,
                    detail: None,
                    warning: None,
                    error: Some("Worker thread panicked".to_string()),
                    duration_ms: 0,
                })
            })
            .collect()
    });

    let label = if results.is_empty() {
        "No agents configured".to_string()
    } else {
        results
            .iter()
            .map(|r| match r.used_percent {
                Some(pct) => format!("{}: {}%", r.name, pct),
                None => format!("{}: ERR", r.name),
            })
            .collect::<Vec<_>>()
            .join(&config.separator)
    };

    let ok_count = results.iter().filter(|r| r.status == "ok").count();
    let failed_count = results.len().saturating_sub(ok_count);
    let max_used_percent = results
        .iter()
        .filter_map(|r| r.used_percent)
        .max()
        .unwrap_or(0);

    let overall_status = if results.is_empty() {
        OverallStatus::Ok
    } else {
        compute_overall_status(ok_count, failed_count, max_used_percent)
    };

    let updated_at_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    StatusReport {
        schema_version: SCHEMA_VERSION,
        label,
        status: overall_status,
        updated_at_epoch,
        summary: SummaryStats {
            max_used_percent,
            active_provider_count: ok_count,
            has_auth_error: false,
        },
        providers: Vec::new(),
        agents: results,
    }
}

fn query_single_agent(agent: &AgentConfig, timeout: Duration) -> AgentStatusResult {
    let start = Instant::now();
    let outcome = execute_and_parse(agent, timeout);
    let duration_ms = start.elapsed().as_millis() as u64;

    match outcome {
        Ok(usage) => AgentStatusResult {
            name: agent.name.clone(),
            agent_type: agent.agent_type.as_str().to_string(),
            status: "ok".to_string(),
            used_percent: Some(usage.used_percent),
            remaining_percent: Some(usage.remaining_percent),
            reset_info: usage.reset_info,
            detail: usage.detail,
            warning: usage.warning,
            error: None,
            duration_ms,
        },
        Err(err) => AgentStatusResult {
            name: agent.name.clone(),
            agent_type: agent.agent_type.as_str().to_string(),
            status: "error".to_string(),
            used_percent: None,
            remaining_percent: None,
            reset_info: None,
            detail: None,
            warning: None,
            error: Some(crate::http::redact_secrets(&format!("{:#}", err))),
            duration_ms,
        },
    }
}

fn execute_and_parse(agent: &AgentConfig, timeout: Duration) -> Result<crate::parser::ParsedUsage> {
    let bin_path = resolve_binary(agent);
    let mut spec = CommandSpec::new(bin_path, timeout);

    match agent.agent_type {
        AgentType::Agy => {
            spec = spec.args(["-p", "/usage", "--output-format", "json"]);
            if let Some(ref cfg_dir) = agent.path {
                if !cfg_dir.trim().is_empty() {
                    let expanded = expand_tilde(cfg_dir);
                    if expanded.join("antigravity-cli").is_dir() {
                        spec = spec.env(
                            "ANTIGRAVITY_CLI_APP_DATA_DIR",
                            expanded.join("antigravity-cli").to_string_lossy(),
                        );
                    } else if expanded
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n == "antigravity-cli")
                    {
                        spec = spec.env(
                            "ANTIGRAVITY_CLI_APP_DATA_DIR",
                            expanded.to_string_lossy(),
                        );
                    }
                }
            }
        }
        AgentType::Claude => {
            spec = spec.args(["-p", "/usage"]);
            if let Some(ref cfg_dir) = agent.path {
                if !cfg_dir.trim().is_empty() {
                    spec = spec.env(
                        "CLAUDE_CONFIG_DIR",
                        expand_tilde(cfg_dir).to_string_lossy(),
                    );
                }
            }
        }
        AgentType::Codex
        | AgentType::Grok
        | AgentType::Cursor
        | AgentType::Openrouter
        | AgentType::Copilot
        | AgentType::Gemini
        | AgentType::Windsurf
        | AgentType::Deepseek => {
            return Err(anyhow!(
                "Provider '{}' requires v2 provider collector",
                agent.agent_type.as_str()
            ));
        }
    }

    let output = run_command(&spec)?;
    let combined = output.combined_text();

    match agent.agent_type {
        AgentType::Agy => {
            if !output.stdout.trim().is_empty() {
                if let Ok(usage) = parse_agy_output(&output.stdout) {
                    return Ok(usage);
                }
            }
            if !output.status.success() {
                return Err(anyhow!(
                    "agy exited with {}: {}",
                    output.status,
                    combined.lines().next().unwrap_or("unknown error")
                ));
            }
            parse_agy_output(&combined)
        }
        AgentType::Claude => {
            if let Ok(usage) = parse_claude_output(&combined) {
                return Ok(usage);
            }
            if !output.status.success() {
                return Err(anyhow!(
                    "claude exited with {}: {}",
                    output.status,
                    combined.lines().next().unwrap_or("unknown error")
                ));
            }
            parse_claude_output(&combined)
        }
        AgentType::Codex
        | AgentType::Grok
        | AgentType::Cursor
        | AgentType::Openrouter
        | AgentType::Copilot
        | AgentType::Gemini
        | AgentType::Windsurf
        | AgentType::Deepseek => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn test_parallel_collect_status_with_mock_claude_and_agy() {
        let tmp_dir =
            std::env::temp_dir().join(format!("gnome-agent-status-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp_dir);
        fs::create_dir_all(&tmp_dir).expect("create temp dir");

        let mock_agy = tmp_dir.join("mock-agy.sh");
        fs::write(
            &mock_agy,
            r#"#!/usr/bin/env bash
cat << 'EOF'
{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":0.84,"reset_time":"2026-11-07T15:56:12Z"}]}]}}}
EOF
"#,
        )
        .expect("write mock agy");
        fs::set_permissions(&mock_agy, fs::Permissions::from_mode(0o755)).expect("chmod mock agy");

        let mock_claude = tmp_dir.join("mock-claude.sh");
        fs::write(
            &mock_claude,
            r#"#!/usr/bin/env bash
if [[ -z "${CLAUDE_CONFIG_DIR:-}" ]]; then
  echo "missing CLAUDE_CONFIG_DIR" >&2
  exit 1
fi
cat << 'EOF'
Warning: this sign-in ends between 2026-11-07 and 2026-11-10 (in 2 days at the earliest).
Current week (all models): 89% used · resets Nov 10, 7am (UTC)
Current week (Fable): 46% used · resets Nov 10, 7am (UTC)
EOF
"#,
        )
        .expect("write mock claude");
        fs::set_permissions(&mock_claude, fs::Permissions::from_mode(0o755))
            .expect("chmod mock claude");

        let cfg = Config {
            auto_discover: false,
            separator: " | ".to_string(),
            timeout_seconds: 10,
            providers: Default::default(),
            agents: vec![
                AgentConfig {
                    name: "AGY".to_string(),
                    agent_type: AgentType::Agy,
                    path: None,
                    binary: Some(mock_agy.to_string_lossy().into_owned()),
                },
                AgentConfig {
                    name: "Claude".to_string(),
                    agent_type: AgentType::Claude,
                    path: Some("~/.claude-work".to_string()),
                    binary: Some(mock_claude.to_string_lossy().into_owned()),
                },
            ],
        };

        let report = collect_status(&cfg);
        let _ = fs::remove_dir_all(&tmp_dir);

        assert_eq!(report.status, OverallStatus::Warning);
        assert_eq!(report.label, "AGY: 16% | Claude: 89%");
        assert_eq!(report.agents.len(), 2);
        assert_eq!(report.agents[0].used_percent, Some(16));
        assert_eq!(report.agents[1].used_percent, Some(89));
    }
}
