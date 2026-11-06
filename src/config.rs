use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    Agy,
    Claude,
}

impl AgentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentType::Agy => "agy",
            AgentType::Claude => "claude",
        }
    }

    pub fn default_binary_name(&self) -> &'static str {
        match self {
            AgentType::Agy => "agy",
            AgentType::Claude => "claude",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Display name shown in the GNOME top bar (e.g. "AGY", "Claude")
    #[serde(alias = "display_name")]
    pub name: String,

    /// Agent type: "agy" or "claude"
    #[serde(rename = "type", alias = "agent_type")]
    pub agent_type: AgentType,

    /// Configuration directory path (e.g. "~/.claude-work" for claude).
    /// Optional for agy.
    #[serde(default)]
    pub path: Option<String>,

    /// Optional explicit binary path (e.g. "/usr/local/bin/claude").
    /// If omitted, searches ~/.local/bin, /usr/local/bin, and PATH.
    #[serde(default)]
    pub binary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Separator between agent labels in the top bar
    #[serde(default = "default_separator")]
    pub separator: String,

    /// Per-agent command timeout in seconds
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,

    /// Configured agents to query in parallel
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
}

fn default_separator() -> String {
    " | ".to_string()
}

fn default_timeout_seconds() -> u64 {
    30
}

impl Default for Config {
    fn default() -> Self {
        Self {
            separator: default_separator(),
            timeout_seconds: default_timeout_seconds(),
            agents: vec![
                AgentConfig {
                    name: "Claude".to_string(),
                    agent_type: AgentType::Claude,
                    path: Some("~/.claude".to_string()),
                    binary: None,
                },
                AgentConfig {
                    name: "AGY".to_string(),
                    agent_type: AgentType::Agy,
                    path: Some("~/.gemini".to_string()),
                    binary: None,
                },
            ],
        }
    }
}

impl Config {
    pub fn default_config_path() -> PathBuf {
        if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
            if !xdg.trim().is_empty() {
                return PathBuf::from(xdg)
                    .join("gnome-agent-status")
                    .join("config.toml");
            }
        }
        let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".config")
            .join("gnome-agent-status")
            .join("config.toml")
    }

    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = match path {
            Some(p) => expand_tilde_path(p),
            None => Self::default_config_path(),
        };

        if !config_path.exists() {
            // If the user explicitly passed a path that doesn't exist, return an error.
            if path.is_some() {
                anyhow::bail!("Configuration file not found at {}", config_path.display());
            }
            // Otherwise fall back to default configuration so it works out of the box.
            return Ok(Self::default());
        }

        let raw = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config file {}", config_path.display()))?;

        if config_path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            serde_json::from_str(&raw)
                .with_context(|| format!("Failed to parse JSON config {}", config_path.display()))
        } else {
            toml::from_str(&raw)
                .with_context(|| format!("Failed to parse TOML config {}", config_path.display()))
        }
    }

    pub fn sample_toml() -> &'static str {
        r#"# gnome-agent-status configuration
# Location: ~/.config/gnome-agent-status/config.toml

separator = " | "
timeout_seconds = 30

[[agents]]
name = "Claude"
type = "claude"
path = "~/.claude"

[[agents]]
name = "AGY"
type = "agy"
path = "~/.gemini"
"#
    }
}

pub fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

/// Expands leading `~` or `~/` to the user's home directory.
pub fn expand_tilde(input: &str) -> PathBuf {
    let trimmed = input.trim();
    if trimmed == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(trimmed)
}

pub fn expand_tilde_path(path: &Path) -> PathBuf {
    match path.to_str() {
        Some(s) => expand_tilde(s),
        None => path.to_path_buf(),
    }
}

/// Resolves the executable path for an agent.
/// Because GNOME Shell often runs with a minimal PATH that omits `~/.local/bin`,
/// this checks explicit `binary` config first, then `~/.local/bin`, `/usr/local/bin`,
/// `/usr/bin`, and finally `PATH`.
pub fn resolve_binary(agent: &AgentConfig) -> PathBuf {
    if let Some(ref custom_bin) = agent.binary {
        if !custom_bin.trim().is_empty() {
            return expand_tilde(custom_bin);
        }
    }

    let bin_name = agent.agent_type.default_binary_name();
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(home) = home_dir() {
        candidates.push(home.join(".local").join("bin").join(bin_name));
        candidates.push(home.join(".cargo").join("bin").join(bin_name));
    }
    candidates.push(PathBuf::from("/usr/local/bin").join(bin_name));
    candidates.push(PathBuf::from("/usr/bin").join(bin_name));

    if let Some(path_os) = env::var_os("PATH") {
        for dir in env::split_paths(&path_os) {
            candidates.push(dir.join(bin_name));
        }
    }

    for candidate in candidates {
        if candidate.is_file() {
            return candidate;
        }
    }

    PathBuf::from(bin_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sample_toml() {
        let cfg: Config = toml::from_str(Config::sample_toml()).expect("valid sample toml");
        assert_eq!(cfg.separator, " | ");
        assert_eq!(cfg.timeout_seconds, 30);
        assert_eq!(cfg.agents.len(), 2);
        assert_eq!(cfg.agents[0].name, "Claude");
        assert_eq!(cfg.agents[0].agent_type, AgentType::Claude);
        assert_eq!(cfg.agents[0].path.as_deref(), Some("~/.claude"));
        assert_eq!(cfg.agents[1].name, "AGY");
        assert_eq!(cfg.agents[1].agent_type, AgentType::Agy);
        assert_eq!(cfg.agents[1].path.as_deref(), Some("~/.gemini"));
    }

    #[test]
    fn test_expand_tilde() {
        let expanded = expand_tilde("~/.claude-work");
        if let Some(home) = home_dir() {
            assert_eq!(expanded, home.join(".claude-work"));
        }
    }
}
