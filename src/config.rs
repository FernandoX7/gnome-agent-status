//! Configuration loader, v1/v2 compatibility mapper, read-only profile discovery,
//! and comment-preserving TOML round-trip helpers for `config` CLI subcommands.

use crate::model::CANONICAL_PROVIDER_IDS;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    Agy,
    Claude,
    Codex,
    Grok,
    Cursor,
}

impl AgentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Agy => "agy",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::Cursor => "cursor",
        }
    }

    pub fn default_binary_name(&self) -> &'static str {
        self.as_str()
    }

    pub fn from_provider_id(id: &str) -> Option<Self> {
        match id {
            "agy" => Some(Self::Agy),
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "grok" => Some(Self::Grok),
            "cursor" => Some(Self::Cursor),
            _ => None,
        }
    }
}

/// Polling mode for a configured account.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PollMode {
    #[default]
    Active,
    Passive,
}

impl PollMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Passive => "passive",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "passive" => Ok(Self::Passive),
            other => Err(anyhow!(
                "Invalid poll_mode '{}': expected 'active' or 'passive'",
                other
            )),
        }
    }
}

/// Manual account entry under `[[providers.<id>.accounts]]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualAccountConfig {
    pub id: String,

    #[serde(default)]
    pub label: Option<String>,

    #[serde(default)]
    pub path: Option<String>,

    #[serde(default)]
    pub binary: Option<String>,

    #[serde(default)]
    pub poll_mode: PollMode,

    #[serde(default)]
    pub switch_command: Option<String>,
}

impl ManualAccountConfig {
    pub fn display_label(&self) -> String {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.id)
            .to_string()
    }
}

/// Per-provider configuration table under `[providers.<id>]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default)]
    pub accounts: Vec<ManualAccountConfig>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            accounts: Vec::new(),
        }
    }
}

/// Backwards-compatible v0.1 `[[agents]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Display name shown in the GNOME top bar (e.g. "AGY", "Claude")
    #[serde(alias = "display_name")]
    pub name: String,

    /// Agent type: "agy", "claude", "codex", "grok", or "cursor"
    #[serde(rename = "type", alias = "agent_type")]
    pub agent_type: AgentType,

    /// Configuration directory or credential file path (e.g. "~/.claude-work").
    #[serde(default)]
    pub path: Option<String>,

    /// Optional explicit binary path (e.g. "/usr/local/bin/claude").
    #[serde(default)]
    pub binary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Whether to automatically discover installed local accounts on disk
    #[serde(default = "default_true")]
    pub auto_discover: bool,

    /// Separator between provider labels in the top bar
    #[serde(default = "default_separator")]
    pub separator: String,

    /// Per-provider/command timeout in seconds
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,

    /// v2 structured per-provider settings and manual account locations
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,

    /// Legacy v1 `[[agents]]` list (mapped seamlessly into provider accounts)
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
}

fn default_true() -> bool {
    true
}

fn default_separator() -> String {
    " | ".to_string()
}

fn default_timeout_seconds() -> u64 {
    30
}

impl Default for Config {
    fn default() -> Self {
        let mut providers = BTreeMap::new();
        for &id in CANONICAL_PROVIDER_IDS {
            providers.insert(id.to_string(), ProviderConfig::default());
        }
        Self {
            auto_discover: true,
            separator: default_separator(),
            timeout_seconds: default_timeout_seconds(),
            providers,
            agents: Vec::new(),
        }
    }
}

/// JSON summary emitted by `gnome-agent-status config show` and mutating `config` subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigShowSummary {
    pub config_path: String,
    pub auto_discover: bool,
    pub timeout_seconds: u64,
    pub separator: String,
    pub manual_accounts: Vec<ManualAccountSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualAccountSummary {
    pub provider: String,
    pub id: String,
    pub label: String,
    pub path: Option<String>,
    pub binary: Option<String>,
    pub poll_mode: String,
    pub switch_command: Option<String>,
}

/// Parameters for `config upsert-account`.
#[derive(Debug, Clone)]
pub struct UpsertAccountRequest {
    pub provider: String,
    pub id: String,
    pub label: String,
    pub path: Option<String>,
    pub binary: Option<String>,
    pub poll_mode: Option<PollMode>,
    pub switch_command: Option<String>,
}

/// Read-only filesystem candidate discovered during `auto_discover`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredLocation {
    pub suggested_id: String,
    pub path: PathBuf,
    pub source_label: String,
    pub is_system_default: bool,
    pub poll_mode: PollMode,
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

    pub fn resolve_config_path(path: Option<&Path>) -> PathBuf {
        match path {
            Some(p) => expand_tilde_path(p),
            None => Self::default_config_path(),
        }
    }

    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = Self::resolve_config_path(path);

        if !config_path.exists() {
            if path.is_some() {
                anyhow::bail!("Configuration file not found at {}", config_path.display());
            }
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

    /// Returns warnings for unknown keys under `[providers.<unknown>]` without failing config load.
    pub fn unknown_provider_warnings(&self) -> Vec<String> {
        self.providers
            .keys()
            .filter(|k| !CANONICAL_PROVIDER_IDS.contains(&k.as_str()))
            .map(|k| format!("Unknown provider '{}' in [providers.{}] ignored", k, k))
            .collect()
    }

    /// Returns `(is_enabled, merged_manual_accounts)` for `provider_id`, combining
    /// v2 `[[providers.<id>.accounts]]` and legacy v1 `[[agents]]`.
    pub fn effective_provider_accounts(&self, provider_id: &str) -> (bool, Vec<ManualAccountConfig>) {
        if let Some(p_cfg) = self.providers.get(provider_id) {
            if !p_cfg.enabled {
                return (false, Vec::new());
            }
        }

        let mut merged: Vec<ManualAccountConfig> = self
            .providers
            .get(provider_id)
            .map(|p| p.accounts.clone())
            .unwrap_or_default();

        // Map legacy v1 `[[agents]]` entries for this provider without duplicating existing paths/ids.
        for (idx, legacy) in self
            .agents
            .iter()
            .filter(|a| a.agent_type.as_str() == provider_id)
            .enumerate()
        {
            let slug_base = slugify_id(&legacy.name);
            let candidate_id = if slug_base.is_empty() {
                format!("legacy-{}", idx + 1)
            } else {
                slug_base
            };

            let dup_by_path = match (&legacy.path, !merged.is_empty()) {
                (Some(lp), true) => {
                    let norm_lp = expand_tilde(lp);
                    merged.iter().any(|m| {
                        m.path
                            .as_deref()
                            .is_some_and(|mp| expand_tilde(mp) == norm_lp)
                    })
                }
                _ => false,
            };

            if dup_by_path {
                continue;
            }

            let unique_id = if merged.iter().any(|m| m.id == candidate_id) {
                format!("{}-{}", candidate_id, idx + 1)
            } else {
                candidate_id
            };

            merged.push(ManualAccountConfig {
                id: unique_id,
                label: Some(legacy.name.clone()),
                path: legacy.path.clone(),
                binary: legacy.binary.clone(),
                poll_mode: PollMode::Active,
                switch_command: None,
            });
        }

        (true, merged)
    }

    /// Builds the JSON summary structure returned by `config show` and mutating `config` commands.
    pub fn to_show_summary(&self, config_path: &Path) -> ConfigShowSummary {
        let mut manual_accounts = Vec::new();

        // Preserve canonical provider order first, then any extra keys.
        let mut ordered_providers: Vec<&str> = CANONICAL_PROVIDER_IDS.to_vec();
        for key in self.providers.keys() {
            if !ordered_providers.contains(&key.as_str()) {
                ordered_providers.push(key.as_str());
            }
        }

        for provider_id in ordered_providers {
            let (_, accounts) = self.effective_provider_accounts(provider_id);
            for acct in accounts {
                let expanded_path = acct
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|p| expand_tilde(p).to_string_lossy().into_owned());
                let binary = acct
                    .binary
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                let switch_command = acct
                    .switch_command
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from);

                manual_accounts.push(ManualAccountSummary {
                    provider: provider_id.to_string(),
                    id: acct.id.clone(),
                    label: acct.display_label(),
                    path: expanded_path,
                    binary,
                    poll_mode: acct.poll_mode.as_str().to_string(),
                    switch_command,
                });
            }
        }

        ConfigShowSummary {
            config_path: config_path.to_string_lossy().into_owned(),
            auto_discover: self.auto_discover,
            timeout_seconds: self.timeout_seconds,
            separator: self.separator.clone(),
            manual_accounts,
        }
    }

    pub fn show_summary(path: Option<&Path>) -> Result<ConfigShowSummary> {
        let target = Self::resolve_config_path(path);
        let cfg = if target.exists() {
            Self::load(Some(&target))?
        } else {
            Self::default()
        };
        Ok(cfg.to_show_summary(&target))
    }

    /// Updates top-level settings (`auto_discover`, `timeout_seconds`) in `config.toml`
    /// while preserving comments and all other sections.
    pub fn update_settings(
        path: Option<&Path>,
        auto_discover: Option<bool>,
        timeout_seconds: Option<u64>,
    ) -> Result<ConfigShowSummary> {
        let target = Self::resolve_config_path(path);
        let mut doc = load_or_init_toml_doc(&target)?;

        if let Some(ad) = auto_discover {
            doc["auto_discover"] = Item::Value(Value::from(ad));
        }
        if let Some(ts) = timeout_seconds {
            doc["timeout_seconds"] = Item::Value(Value::from(ts.max(1) as i64));
        }

        write_toml_doc_atomically(&target, &doc)?;
        Self::show_summary(Some(&target))
    }

    /// Creates or updates a `[[providers.<provider>.accounts]]` entry in `config.toml`,
    /// preserving comments and other config sections.
    pub fn upsert_account(
        path: Option<&Path>,
        req: UpsertAccountRequest,
    ) -> Result<ConfigShowSummary> {
        let provider = req.provider.trim().to_ascii_lowercase();
        if !CANONICAL_PROVIDER_IDS.contains(&provider.as_str()) {
            anyhow::bail!(
                "Unsupported provider '{}': expected one of {}",
                provider,
                CANONICAL_PROVIDER_IDS.join(", ")
            );
        }
        let id = req.id.trim().to_string();
        if id.is_empty() {
            anyhow::bail!("Account --id must not be empty");
        }
        let label = req.label.trim().to_string();
        if label.is_empty() {
            anyhow::bail!("Account --label must not be empty");
        }

        let target = Self::resolve_config_path(path);
        let mut doc = load_or_init_toml_doc(&target)?;

        if !doc.contains_key("providers") || !doc["providers"].is_table_like() {
            let mut providers_tbl = Table::new();
            providers_tbl.set_implicit(true);
            doc["providers"] = Item::Table(providers_tbl);
        }

        let providers_tbl = doc["providers"]
            .as_table_like_mut()
            .ok_or_else(|| anyhow!("'providers' in config.toml is not a table"))?;

        if !providers_tbl.contains_key(&provider) {
            let mut p_tbl = Table::new();
            p_tbl["enabled"] = Item::Value(Value::from(true));
            providers_tbl.insert(&provider, Item::Table(p_tbl));
        }

        let provider_tbl = providers_tbl
            .get_mut(&provider)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| anyhow!("'[providers.{}]' is not a table", provider))?;

        if !provider_tbl.contains_key("accounts") {
            provider_tbl.insert("accounts", Item::ArrayOfTables(ArrayOfTables::new()));
        }

        let accounts_aot = provider_tbl
            .get_mut("accounts")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| anyhow!("'providers.{}.accounts' is not an array of tables", provider))?;

        let existing = accounts_aot
            .iter_mut()
            .find(|t| t.get("id").and_then(Item::as_str) == Some(id.as_str()));

        let apply_fields = |tbl: &mut Table, is_new: bool| {
            tbl["id"] = Item::Value(Value::from(id.as_str()));
            tbl["label"] = Item::Value(Value::from(label.as_str()));

            if let Some(ref p) = req.path {
                if p.trim().is_empty() {
                    tbl.remove("path");
                } else {
                    tbl["path"] = Item::Value(Value::from(p.trim()));
                }
            }
            if let Some(ref b) = req.binary {
                if b.trim().is_empty() {
                    tbl.remove("binary");
                } else {
                    tbl["binary"] = Item::Value(Value::from(b.trim()));
                }
            }
            if let Some(pm) = req.poll_mode {
                tbl["poll_mode"] = Item::Value(Value::from(pm.as_str()));
            } else if is_new && !tbl.contains_key("poll_mode") {
                tbl["poll_mode"] = Item::Value(Value::from(PollMode::Active.as_str()));
            }
            if let Some(ref sc) = req.switch_command {
                if sc.trim().is_empty() {
                    tbl.remove("switch_command");
                } else {
                    tbl["switch_command"] = Item::Value(Value::from(sc.trim()));
                }
            }
        };

        if let Some(tbl) = existing {
            apply_fields(tbl, false);
        } else {
            let mut new_tbl = Table::new();
            apply_fields(&mut new_tbl, true);
            accounts_aot.push(new_tbl);
        }

        write_toml_doc_atomically(&target, &doc)?;
        Self::show_summary(Some(&target))
    }

    /// Removes a manual account entry from `[[providers.<provider>.accounts]]` (and matching
    /// legacy `[[agents]]` if present) and returns updated `config show` summary.
    pub fn remove_account(
        path: Option<&Path>,
        provider: &str,
        account_id: &str,
    ) -> Result<ConfigShowSummary> {
        let provider = provider.trim().to_ascii_lowercase();
        let target_id = account_id.trim();
        let target = Self::resolve_config_path(path);

        if !target.exists() {
            return Self::show_summary(Some(&target));
        }

        let mut doc = load_or_init_toml_doc(&target)?;

        if let Some(accounts_aot) = doc
            .get_mut("providers")
            .and_then(Item::as_table_like_mut)
            .and_then(|p| p.get_mut(&provider))
            .and_then(Item::as_table_like_mut)
            .and_then(|pt| pt.get_mut("accounts"))
            .and_then(Item::as_array_of_tables_mut)
        {
            let mut idx = 0;
            while idx < accounts_aot.len() {
                let matches = accounts_aot
                    .get(idx)
                    .and_then(|t| t.get("id"))
                    .and_then(Item::as_str)
                    == Some(target_id);
                if matches {
                    accounts_aot.remove(idx);
                } else {
                    idx += 1;
                }
            }
        }

        // Also remove from legacy `[[agents]]` if the user is removing a migrated v1 entry.
        if let Some(agents_aot) = doc.get_mut("agents").and_then(Item::as_array_of_tables_mut) {
            let mut idx = 0;
            while idx < agents_aot.len() {
                let matches = agents_aot.get(idx).is_some_and(|t| {
                    let t_type = t
                        .get("type")
                        .or_else(|| t.get("agent_type"))
                        .and_then(Item::as_str)
                        .unwrap_or_default();
                    let t_name = t
                        .get("name")
                        .or_else(|| t.get("display_name"))
                        .and_then(Item::as_str)
                        .unwrap_or_default();
                    t_type.eq_ignore_ascii_case(&provider)
                        && (slugify_id(t_name) == target_id || t_name == target_id)
                });
                if matches {
                    agents_aot.remove(idx);
                } else {
                    idx += 1;
                }
            }
        }

        write_toml_doc_atomically(&target, &doc)?;
        Self::show_summary(Some(&target))
    }

    pub fn sample_toml() -> &'static str {
        r#"# gnome-agent-status configuration
# Location: ~/.config/gnome-agent-status/config.toml

auto_discover = true
timeout_seconds = 30
separator = " | "

[providers.claude]
enabled = true

[providers.codex]
enabled = true

[providers.grok]
enabled = true

[providers.agy]
enabled = true

[providers.cursor]
enabled = true
"#
    }
}

fn load_or_init_toml_doc(path: &Path) -> Result<DocumentMut> {
    if path.exists() {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        raw.parse::<DocumentMut>()
            .with_context(|| format!("Failed to parse TOML in {}", path.display()))
    } else {
        Config::sample_toml()
            .parse::<DocumentMut>()
            .context("Failed to initialize default TOML document")
    }
}

fn write_toml_doc_atomically(target: &Path, doc: &DocumentMut) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }

    let existing_mode = fs::metadata(target).ok().map(|m| {
        #[cfg(unix)]
        {
            m.permissions().mode() & 0o777
        }
        #[cfg(not(unix))]
        {
            let _ = m;
            0o600u32
        }
    });

    let tmp_name = format!(
        ".{}.tmp.{}.{}",
        target
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("config.toml"),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp_path = target.with_file_name(tmp_name);

    let serialized = doc.to_string();
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            opts.mode(existing_mode.unwrap_or(0o600));
        }
        let mut file = opts
            .open(&tmp_path)
            .with_context(|| format!("Failed to create temp config {}", tmp_path.display()))?;
        file.write_all(serialized.as_bytes())?;
        file.sync_all()?;
    }

    fs::rename(&tmp_path, target).with_context(|| {
        let _ = fs::remove_file(&tmp_path);
        format!("Failed to replace config file {}", target.display())
    })?;

    Ok(())
}

pub fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

pub fn xdg_config_dir(home: Option<&Path>) -> Option<PathBuf> {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            return Some(PathBuf::from(xdg));
        }
    }
    home.map(|h| h.join(".config"))
}

/// Expands leading `~` or `~/` to the user's home directory.
pub fn expand_tilde(input: &str) -> PathBuf {
    expand_tilde_with_home(input, home_dir().as_deref())
}

pub fn expand_tilde_with_home(input: &str, home: Option<&Path>) -> PathBuf {
    let trimmed = input.trim();
    if trimmed == "~" {
        if let Some(h) = home {
            return h.to_path_buf();
        }
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Some(h) = home {
            return h.join(rest);
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

/// Abbreviates a path under `home` to `~/...` for concise UI display (`source_label`).
pub fn abbreviate_home(path: &Path, home: Option<&Path>) -> String {
    if let Some(h) = home {
        if path == h {
            return "~".to_string();
        }
        if let Ok(rel) = path.strip_prefix(h) {
            return format!("~/{}", rel.display());
        }
    }
    path.to_string_lossy().into_owned()
}

pub fn slugify_id(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Resolves the executable path for a provider or custom binary path.
/// Checks explicit `custom_binary` first, then `~/.local/bin`, `~/.cargo/bin`,
/// `/usr/local/bin`, `/usr/bin`, `/opt/homebrew/bin`, and `PATH`.
pub fn resolve_binary_for(default_bin_name: &str, custom_binary: Option<&str>) -> PathBuf {
    if let Some(custom) = custom_binary {
        if !custom.trim().is_empty() {
            return expand_tilde(custom);
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        candidates.push(home.join(".local").join("bin").join(default_bin_name));
        candidates.push(home.join(".cargo").join("bin").join(default_bin_name));
        candidates.push(home.join(".npm-global").join("bin").join(default_bin_name));
    }
    candidates.push(PathBuf::from("/usr/local/bin").join(default_bin_name));
    candidates.push(PathBuf::from("/usr/bin").join(default_bin_name));
    candidates.push(PathBuf::from("/opt/homebrew/bin").join(default_bin_name));

    if let Some(path_os) = env::var_os("PATH") {
        for dir in env::split_paths(&path_os) {
            candidates.push(dir.join(default_bin_name));
        }
    }

    for candidate in candidates {
        if candidate.is_file() {
            return candidate;
        }
    }

    PathBuf::from(default_bin_name)
}

pub fn resolve_binary(agent: &AgentConfig) -> PathBuf {
    resolve_binary_for(agent.agent_type.default_binary_name(), agent.binary.as_deref())
}

/// Performs non-destructive, read-only auto-discovery of local credential directories/files
/// for `provider_id` under `home` and `xdg_config`.
pub fn discover_provider_locations(
    provider_id: &str,
    home: Option<&Path>,
    xdg_config: Option<&Path>,
) -> Vec<DiscoveredLocation> {
    let Some(home) = home else {
        return Vec::new();
    };
    let mut out = Vec::new();

    match provider_id {
        "claude" => {
            let default_dir = home.join(".claude");
            if default_dir.is_dir()
                && (default_dir.join(".credentials.json").is_file()
                    || default_dir.join(".claude.json").is_file()
                    || home.join(".claude.json").is_file())
            {
                out.push(DiscoveredLocation {
                    suggested_id: "default".to_string(),
                    source_label: abbreviate_home(&default_dir, Some(home)),
                    path: default_dir,
                    is_system_default: true,
                    poll_mode: PollMode::Active,
                });
            }

            for dir in scan_prefixed_dirs(home, &[".claude-", ".claude_"]) {
                if dir.join(".credentials.json").is_file() || dir.join(".claude.json").is_file() {
                    let suffix = dir
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("profile")
                        .trim_start_matches(".claude-")
                        .trim_start_matches(".claude_");
                    out.push(DiscoveredLocation {
                        suggested_id: slugify_id(suffix),
                        source_label: abbreviate_home(&dir, Some(home)),
                        path: dir,
                        is_system_default: false,
                        poll_mode: PollMode::Active,
                    });
                }
            }
        }
        "codex" => {
            let default_dir = home.join(".codex");
            if default_dir.is_dir()
                && (default_dir.join("auth.json").is_file()
                    || default_dir.join("config.toml").is_file()
                    || default_dir.join("sessions").is_dir())
            {
                out.push(DiscoveredLocation {
                    suggested_id: "default".to_string(),
                    source_label: abbreviate_home(&default_dir, Some(home)),
                    path: default_dir,
                    is_system_default: true,
                    poll_mode: PollMode::Active,
                });
            }

            for dir in scan_prefixed_dirs(home, &[".codex-", ".codex_"]) {
                if dir.join("auth.json").is_file()
                    || dir.join("config.toml").is_file()
                    || dir.join("sessions").is_dir()
                {
                    let suffix = dir
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("profile")
                        .trim_start_matches(".codex-")
                        .trim_start_matches(".codex_");
                    out.push(DiscoveredLocation {
                        suggested_id: slugify_id(suffix),
                        source_label: abbreviate_home(&dir, Some(home)),
                        path: dir,
                        is_system_default: false,
                        poll_mode: PollMode::Active,
                    });
                }
            }

            let opencode_auth = home
                .join(".local")
                .join("share")
                .join("opencode")
                .join("auth.json");
            if opencode_auth.is_file() {
                out.push(DiscoveredLocation {
                    suggested_id: "opencode".to_string(),
                    source_label: abbreviate_home(&opencode_auth, Some(home)),
                    path: opencode_auth,
                    is_system_default: false,
                    poll_mode: PollMode::Active,
                });
            }
        }
        "grok" => {
            let default_dir = home.join(".grok");
            if default_dir.is_dir()
                && (default_dir.join("auth.json").is_file()
                    || default_dir.join("config.toml").is_file())
            {
                out.push(DiscoveredLocation {
                    suggested_id: "default".to_string(),
                    source_label: abbreviate_home(&default_dir, Some(home)),
                    path: default_dir,
                    is_system_default: true,
                    poll_mode: PollMode::Active,
                });
            }

            for dir in scan_prefixed_dirs(home, &[".grok-", ".grok_"]) {
                let fname = dir.file_name().and_then(|s| s.to_str()).unwrap_or_default();
                if fname == ".grok-swap" {
                    continue;
                }
                if dir.join("auth.json").is_file() {
                    let suffix = fname
                        .trim_start_matches(".grok-")
                        .trim_start_matches(".grok_");
                    out.push(DiscoveredLocation {
                        suggested_id: slugify_id(suffix),
                        source_label: abbreviate_home(&dir, Some(home)),
                        path: dir,
                        is_system_default: false,
                        poll_mode: PollMode::Active,
                    });
                }
            }

            let swap_accounts_dir = home.join(".grok-swap").join("accounts");
            if let Ok(entries) = fs::read_dir(&swap_accounts_dir) {
                let mut subdirs: Vec<PathBuf> = entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() && p.join("auth.json").is_file())
                    .collect();
                subdirs.sort();
                for sub in subdirs {
                    let slot = sub
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("slot");
                    out.push(DiscoveredLocation {
                        suggested_id: format!("swap-{}", slugify_id(slot)),
                        source_label: abbreviate_home(&sub, Some(home)),
                        path: sub,
                        is_system_default: false,
                        poll_mode: PollMode::Active,
                    });
                }
            }
        }
        "agy" => {
            let gemini_dir = home.join(".gemini");
            let has_agy_dir = gemini_dir.join("antigravity").is_dir()
                || gemini_dir.join("antigravity-cli").is_dir();
            let has_agy_bin = resolve_binary_for("agy", None).is_file();
            if has_agy_dir || has_agy_bin {
                let default_path = if gemini_dir.join("antigravity-cli").is_dir() {
                    gemini_dir.join("antigravity-cli")
                } else {
                    gemini_dir.join("antigravity")
                };
                out.push(DiscoveredLocation {
                    suggested_id: "default".to_string(),
                    source_label: abbreviate_home(&default_path, Some(home)),
                    path: default_path,
                    is_system_default: true,
                    poll_mode: PollMode::Active,
                });
            }

            // Passive switcher profile metadata directories (never mutate keyring!).
            let mut switcher_roots = vec![home.join(".agy-auth").join("accounts")];
            if let Some(xdg) = xdg_config {
                switcher_roots.push(xdg.join("agy-auth").join("accounts"));
            }
            for root in switcher_roots {
                if let Ok(entries) = fs::read_dir(&root) {
                    let mut slots: Vec<PathBuf> =
                        entries.filter_map(Result::ok).map(|e| e.path()).collect();
                    slots.sort();
                    for slot_path in slots {
                        if slot_path.is_dir() || slot_path.is_file() {
                            let name = slot_path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("slot");
                            out.push(DiscoveredLocation {
                                suggested_id: slugify_id(name),
                                source_label: abbreviate_home(&slot_path, Some(home)),
                                path: slot_path,
                                is_system_default: false,
                                poll_mode: PollMode::Passive,
                            });
                        }
                    }
                }
            }
        }
        "cursor" => {
            let mut candidates = Vec::new();
            if let Some(xdg) = xdg_config {
                candidates.push(
                    xdg.join("Cursor")
                        .join("User")
                        .join("globalStorage")
                        .join("state.vscdb"),
                );
            }
            let linux_default = home
                .join(".config")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb");
            if !candidates.contains(&linux_default) {
                candidates.push(linux_default);
            }
            let macos_default = home
                .join("Library")
                .join("Application Support")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb");
            if !candidates.contains(&macos_default) {
                candidates.push(macos_default);
            }

            for (idx, db_path) in candidates.into_iter().enumerate() {
                if db_path.is_file() {
                    out.push(DiscoveredLocation {
                        suggested_id: if idx == 0 {
                            "default".to_string()
                        } else {
                            format!("default-{}", idx + 1)
                        },
                        source_label: abbreviate_home(&db_path, Some(home)),
                        path: db_path,
                        is_system_default: out.is_empty(),
                        poll_mode: PollMode::Active,
                    });
                }
            }
        }
        _ => {}
    }

    out
}

fn scan_prefixed_dirs(parent: &Path, prefixes: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            if !path.is_dir() {
                return None;
            }
            let name = path.file_name()?.to_str()?;
            if prefixes.iter().any(|p| name.starts_with(p)) {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    dirs.sort();
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_v2_sample_toml() {
        let cfg: Config = toml::from_str(Config::sample_toml()).expect("valid v2 sample toml");
        assert!(cfg.auto_discover);
        assert_eq!(cfg.separator, " | ");
        assert_eq!(cfg.timeout_seconds, 30);
        assert_eq!(cfg.providers.len(), 5);
        assert!(cfg.providers["claude"].enabled);
        assert!(cfg.providers["cursor"].enabled);
        assert!(cfg.unknown_provider_warnings().is_empty());
    }

    #[test]
    fn test_parse_legacy_v1_agents_toml_and_map_to_providers() {
        let v1_toml = r#"
separator = " · "
timeout_seconds = 15

[[agents]]
name = "Claude Work"
type = "claude"
path = "~/.claude-work"

[[agents]]
name = "AGY"
type = "agy"
path = "~/.gemini"
"#;
        let cfg: Config = toml::from_str(v1_toml).expect("parse legacy v1 toml");
        assert_eq!(cfg.separator, " · ");
        assert_eq!(cfg.timeout_seconds, 15);
        assert_eq!(cfg.agents.len(), 2);

        let (enabled, claude_accts) = cfg.effective_provider_accounts("claude");
        assert!(enabled);
        assert_eq!(claude_accts.len(), 1);
        assert_eq!(claude_accts[0].id, "claude-work");
        assert_eq!(claude_accts[0].display_label(), "Claude Work");
        assert_eq!(claude_accts[0].path.as_deref(), Some("~/.claude-work"));
    }

    #[test]
    fn test_unknown_provider_emits_warning_without_failing() {
        let raw = r#"
[providers.claude]
enabled = true

[providers.future_agent]
enabled = true
"#;
        let cfg: Config = toml::from_str(raw).expect("unknown provider should not fail parse");
        let warnings = cfg.unknown_provider_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("future_agent"));
    }

    #[test]
    fn test_config_round_trip_set_upsert_remove_preserves_comments() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "gas-config-rt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp_dir).expect("create tmp dir");
        let cfg_path = tmp_dir.join("config.toml");

        fs::write(
            &cfg_path,
            "# Custom user comment at top\nauto_discover = true\ntimeout_seconds = 30\nseparator = \" | \"\n\n[providers.codex]\nenabled = false\n",
        )
        .expect("seed config");

        let s1 = Config::update_settings(Some(&cfg_path), Some(false), Some(45)).expect("set");
        assert!(!s1.auto_discover);
        assert_eq!(s1.timeout_seconds, 45);

        let s2 = Config::upsert_account(
            Some(&cfg_path),
            UpsertAccountRequest {
                provider: "claude".to_string(),
                id: "work".to_string(),
                label: "Work Claude".to_string(),
                path: Some("/tmp/claude-work".to_string()),
                binary: None,
                poll_mode: Some(PollMode::Passive),
                switch_command: Some("echo switched".to_string()),
            },
        )
        .expect("upsert");
        assert_eq!(s2.manual_accounts.len(), 1);
        assert_eq!(s2.manual_accounts[0].provider, "claude");
        assert_eq!(s2.manual_accounts[0].id, "work");
        assert_eq!(s2.manual_accounts[0].label, "Work Claude");
        assert_eq!(
            s2.manual_accounts[0].path.as_deref(),
            Some("/tmp/claude-work")
        );
        assert_eq!(s2.manual_accounts[0].poll_mode, "passive");
        assert_eq!(
            s2.manual_accounts[0].switch_command.as_deref(),
            Some("echo switched")
        );

        // Update existing entry in-place
        let s3 = Config::upsert_account(
            Some(&cfg_path),
            UpsertAccountRequest {
                provider: "claude".to_string(),
                id: "work".to_string(),
                label: "Work Claude Updated".to_string(),
                path: None,
                binary: None,
                poll_mode: Some(PollMode::Active),
                switch_command: None,
            },
        )
        .expect("upsert update");
        assert_eq!(s3.manual_accounts.len(), 1);
        assert_eq!(s3.manual_accounts[0].label, "Work Claude Updated");
        assert_eq!(s3.manual_accounts[0].poll_mode, "active");

        // Verify comments & [providers.codex] section survived intact
        let disk_text = fs::read_to_string(&cfg_path).expect("read config");
        assert!(disk_text.contains("# Custom user comment at top"));
        let reloaded = Config::load(Some(&cfg_path)).expect("reload");
        assert!(!reloaded.providers["codex"].enabled);

        // Remove account
        let s4 = Config::remove_account(Some(&cfg_path), "claude", "work").expect("remove");
        assert!(s4.manual_accounts.is_empty());

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_discover_provider_locations_in_mock_home() {
        let tmp_home = std::env::temp_dir().join(format!(
            "gas-disc-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let xdg = tmp_home.join(".config");

        fs::create_dir_all(tmp_home.join(".claude")).unwrap();
        fs::write(tmp_home.join(".claude/.credentials.json"), "{}").unwrap();
        fs::create_dir_all(tmp_home.join(".claude-work")).unwrap();
        fs::write(tmp_home.join(".claude-work/.credentials.json"), "{}").unwrap();

        fs::create_dir_all(tmp_home.join(".codex-team")).unwrap();
        fs::write(tmp_home.join(".codex-team/auth.json"), "{}").unwrap();

        fs::create_dir_all(tmp_home.join(".grok-swap/accounts/personal")).unwrap();
        fs::write(tmp_home.join(".grok-swap/accounts/personal/auth.json"), "{}").unwrap();

        let cursor_db = xdg.join("Cursor/User/globalStorage/state.vscdb");
        fs::create_dir_all(cursor_db.parent().unwrap()).unwrap();
        fs::write(&cursor_db, "sqlite").unwrap();

        let claude_locs = discover_provider_locations("claude", Some(&tmp_home), Some(&xdg));
        assert_eq!(claude_locs.len(), 2);
        assert!(claude_locs[0].is_system_default);
        assert_eq!(claude_locs[0].source_label, "~/.claude");
        assert_eq!(claude_locs[1].suggested_id, "work");
        assert_eq!(claude_locs[1].source_label, "~/.claude-work");

        let codex_locs = discover_provider_locations("codex", Some(&tmp_home), Some(&xdg));
        assert_eq!(codex_locs.len(), 1);
        assert_eq!(codex_locs[0].suggested_id, "team");

        let grok_locs = discover_provider_locations("grok", Some(&tmp_home), Some(&xdg));
        assert_eq!(grok_locs.len(), 1);
        assert_eq!(grok_locs[0].suggested_id, "swap-personal");

        let cursor_locs = discover_provider_locations("cursor", Some(&tmp_home), Some(&xdg));
        assert_eq!(cursor_locs.len(), 1);
        assert!(cursor_locs[0].is_system_default);

        let _ = fs::remove_dir_all(&tmp_home);
    }

    #[test]
    fn test_expand_tilde() {
        let expanded = expand_tilde("~/.claude-work");
        if let Some(home) = home_dir() {
            assert_eq!(expanded, home.join(".claude-work"));
        }
    }
}
