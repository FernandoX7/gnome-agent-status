use crate::config::{expand_tilde, resolve_binary, AgentConfig, AgentType, Config};
use crate::parser::{parse_agy_output, parse_claude_output};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    /// Combined top-bar label (e.g. "AGY: 16% | Claude: 89%")
    pub label: String,
    /// Overall status: "ok", "degraded", or "error"
    pub status: String,
    /// Unix epoch timestamp (seconds) when report was generated
    pub updated_at_epoch: u64,
    /// Individual agent results in configured order
    pub agents: Vec<AgentStatusResult>,
}

/// Runs all configured agents in parallel and aggregates their 7-day usage into a `StatusReport`.
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
    let overall_status = if results.is_empty() || ok_count == results.len() {
        "ok"
    } else if ok_count > 0 {
        "degraded"
    } else {
        "error"
    };

    let updated_at_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    StatusReport {
        label,
        status: overall_status.to_string(),
        updated_at_epoch,
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
            error: Some(format!("{:#}", err)),
            duration_ms,
        },
    }
}

fn execute_and_parse(agent: &AgentConfig, timeout: Duration) -> Result<crate::parser::ParsedUsage> {
    let bin_path = resolve_binary(agent);
    let mut cmd = Command::new(&bin_path);

    match agent.agent_type {
        AgentType::Agy => {
            cmd.args(["-p", "/usage", "--output-format", "json"]);
            if let Some(ref cfg_dir) = agent.path {
                if !cfg_dir.trim().is_empty() {
                    let expanded = expand_tilde(cfg_dir);
                    let app_data = if expanded.join("antigravity-cli").is_dir() {
                        expanded.join("antigravity-cli")
                    } else {
                        expanded
                    };
                    cmd.env("ANTIGRAVITY_CLI_APP_DATA_DIR", app_data);
                }
            }
        }
        AgentType::Claude => {
            cmd.args(["-p", "/usage"]);
            if let Some(ref cfg_dir) = agent.path {
                if !cfg_dir.trim().is_empty() {
                    cmd.env("CLAUDE_CONFIG_DIR", expand_tilde(cfg_dir));
                }
            }
        }
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn '{}'", bin_path.display()))?;

    let mut stdout_pipe = child.stdout.take().context("Failed to capture stdout")?;
    let mut stderr_pipe = child.stderr.take().context("Failed to capture stderr")?;

    let stdout_handle = thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout_pipe.read_to_string(&mut buf);
        buf
    });

    let stderr_handle = thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        buf
    });

    let start = Instant::now();
    let poll_interval = Duration::from_millis(50);

    let status = loop {
        if let Some(exit_status) = child.try_wait()? {
            break exit_status;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "Command '{}' timed out after {}s",
                bin_path.display(),
                timeout.as_secs()
            ));
        }
        thread::sleep(poll_interval);
    };

    let stdout_str = stdout_handle.join().unwrap_or_default();
    let stderr_str = stderr_handle.join().unwrap_or_default();

    // Combine stdout and stderr for claude since warnings or usage text may appear on either stream
    let combined = if stderr_str.trim().is_empty() {
        stdout_str.clone()
    } else if stdout_str.trim().is_empty() {
        stderr_str.clone()
    } else {
        format!("{}\n{}", stderr_str.trim(), stdout_str.trim())
    };

    match agent.agent_type {
        AgentType::Agy => {
            if !stdout_str.trim().is_empty() {
                if let Ok(usage) = parse_agy_output(&stdout_str) {
                    return Ok(usage);
                }
            }
            if !status.success() {
                return Err(anyhow!(
                    "agy exited with {}: {}",
                    status,
                    combined.lines().next().unwrap_or("unknown error")
                ));
            }
            parse_agy_output(&combined)
        }
        AgentType::Claude => {
            if let Ok(usage) = parse_claude_output(&combined) {
                return Ok(usage);
            }
            if !status.success() {
                return Err(anyhow!(
                    "claude exited with {}: {}",
                    status,
                    combined.lines().next().unwrap_or("unknown error")
                ));
            }
            parse_claude_output(&combined)
        }
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
            separator: " | ".to_string(),
            timeout_seconds: 10,
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

        assert_eq!(report.status, "ok");
        assert_eq!(report.label, "AGY: 16% | Claude: 89%");
        assert_eq!(report.agents.len(), 2);
        assert_eq!(report.agents[0].used_percent, Some(16));
        assert_eq!(report.agents[1].used_percent, Some(89));
    }
}
