use crate::cache::{atomic_write_0600, CacheStore};
use crate::config::{Config, PollMode, UpsertAccountRequest};
use crate::model::{StatusReport, CANONICAL_PROVIDER_IDS};
use crate::proc_exec::{self, CommandSpec};
use crate::providers::{claude, CollectOptions, FetchContext, ProviderRegistry};
use crate::text;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

/// Command-line interface for `gnome-agent-status` v0.2.
///
/// Running without a subcommand remains 100% backwards-compatible with v0.1:
/// `gnome-agent-status [--config <path>] [--init-config] [--pretty] [--text] [--color]`
#[derive(Debug, Clone, Parser)]
#[command(
    name = "gnome-agent-status",
    version,
    about = "Queries usage limits across configured AI coding agents in parallel and outputs JSON or aligned text."
)]
pub struct Cli {
    /// Path to configuration file (TOML or JSON).
    /// Defaults to `$XDG_CONFIG_HOME/gnome-agent-status/config.toml`.
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    /// Initialize default config file at `~/.config/gnome-agent-status/config.toml` if missing.
    #[arg(long)]
    pub init_config: bool,

    /// Pretty-print JSON output.
    #[arg(long, global = true)]
    pub pretty: bool,

    /// Print aligned plain-text table instead of JSON.
    #[arg(long, global = true)]
    pub text: bool,

    /// Add ANSI colors to `--text` output.
    #[arg(long, global = true)]
    pub color: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Commands {
    /// Refresh expired accounts, atomically update `state.json`, and print `StatusReport` v2 JSON.
    Collect {
        /// Bypass normal TTL while honoring the 30s manual debounce and active HTTP 429 locks.
        #[arg(long)]
        force: bool,

        /// Restrict live refresh to a single canonical provider ID (`claude`, `codex`, `grok`, `agy`, `cursor`).
        #[arg(long)]
        provider: Option<String>,
    },

    /// Read `state.json` / `collector-cache.json`, apply passive time-decay, and print JSON with zero network I/O.
    Cached,

    /// Manage active account selection for a provider.
    Account {
        #[command(subcommand)]
        action: AccountCommand,
    },

    /// Inspect or safely update `config.toml` without losing comments.
    Config {
        #[command(subcommand)]
        action: ConfigCommand,
    },

    /// Ingest passive usage telemetry piped from external CLI hooks on `stdin`.
    IngestHook {
        #[command(subcommand)]
        target: IngestHookCommand,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AccountCommand {
    /// Pin `<account_id>` (or `"default"`) as active for `<provider_id>`, run optional `switch_command`, and print updated `StatusReport` v2 JSON.
    Select {
        /// Canonical provider ID (`claude`, `codex`, `grok`, `agy`, `cursor`).
        provider_id: String,

        /// Account ID (`<provider>:<slug>`, `<slug>`, or `"default"`).
        account_id: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ConfigCommand {
    /// Print JSON summary of current configuration and manual accounts.
    Show,

    /// Update top-level settings in `config.toml` and print updated `config show` JSON.
    Set {
        /// Enable or disable read-only local credential auto-discovery (`true` or `false`).
        #[arg(long, value_name = "BOOL")]
        auto_discover: Option<bool>,

        /// Per-provider subprocess/HTTP timeout in seconds.
        #[arg(long, value_name = "SECONDS")]
        timeout_seconds: Option<u64>,
    },

    /// Create or update a `[[providers.<provider>.accounts]]` entry in `config.toml`.
    UpsertAccount {
        /// Canonical provider ID (`claude`, `codex`, `grok`, `agy`, `cursor`).
        #[arg(long)]
        provider: String,

        /// Short slug identifier for the account (e.g. `work`, `personal`).
        #[arg(long)]
        id: String,

        /// Display label shown in UI and text output.
        #[arg(long)]
        label: String,

        /// Optional custom profile directory or credential file path.
        #[arg(long)]
        path: Option<String>,

        /// Optional custom CLI binary path.
        #[arg(long)]
        binary: Option<String>,

        /// Polling mode (`active` or `passive`).
        #[arg(long)]
        poll_mode: Option<String>,

        /// Optional shell command executed when switching to this account via `account select`.
        #[arg(long)]
        switch_command: Option<String>,
    },

    /// Remove a manual account entry from `config.toml`.
    RemoveAccount {
        /// Canonical provider ID (`claude`, `codex`, `grok`, `agy`, `cursor`).
        #[arg(long)]
        provider: String,

        /// Account slug ID to remove.
        #[arg(long)]
        id: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum IngestHookCommand {
    /// Read Claude Code `statusLine` JSON from `stdin` and atomically update passive `5h`/`wk` buckets.
    Claude {
        /// Optional Claude account ID or slug to target.
        #[arg(long)]
        account: Option<String>,

        /// Print the updated `StatusReport` v2 JSON to `stdout` (silent by default for `statusLine` TUI safety).
        #[arg(long)]
        json: bool,
    },
}

/// Optional runtime overrides used by hermetic tests to isolate cache, filesystem, and clock.
pub struct CliRuntime<'a> {
    pub cache_store: CacheStore,
    pub fetch_ctx_override: Option<FetchContext>,
    pub now: DateTime<Utc>,
    pub stdin_data: Option<&'a str>,
}

impl Default for CliRuntime<'_> {
    fn default() -> Self {
        Self {
            cache_store: CacheStore::from_env(),
            fetch_ctx_override: None,
            now: Utc::now(),
            stdin_data: None,
        }
    }
}

/// Formats `report` according to `--text`, `--color`, and `--pretty`.
pub fn format_report(report: &StatusReport, cli: &Cli) -> Result<String> {
    if cli.text {
        Ok(text::format_text(report, cli.color))
    } else if cli.pretty {
        Ok(serde_json::to_string_pretty(report)?)
    } else {
        Ok(serde_json::to_string(report)?)
    }
}

/// Executes `cli` and returns any `stdout` payload to print.
pub fn execute_cli(cli: &Cli, runtime: &CliRuntime<'_>) -> Result<Option<String>> {
    if cli.init_config {
        let target = Config::resolve_config_path(cli.config.as_deref());
        if !target.exists() {
            atomic_write_0600(&target, Config::sample_toml().as_bytes())
                .with_context(|| format!("Failed to write sample config to {}", target.display()))?;
            eprintln!("Created default config at {}", target.display());
        } else {
            eprintln!("Config already exists at {}", target.display());
        }
        return Ok(None);
    }

    match &cli.command {
        Some(Commands::Config { action }) => {
            let summary = match action {
                ConfigCommand::Show => Config::show_summary(cli.config.as_deref())?,
                ConfigCommand::Set {
                    auto_discover,
                    timeout_seconds,
                } => Config::update_settings(
                    cli.config.as_deref(),
                    *auto_discover,
                    *timeout_seconds,
                )?,
                ConfigCommand::UpsertAccount {
                    provider,
                    id,
                    label,
                    path,
                    binary,
                    poll_mode,
                    switch_command,
                } => {
                    let parsed_mode = match poll_mode.as_deref().map(str::trim) {
                        None | Some("") => None,
                        Some(m) if m.eq_ignore_ascii_case("active") => Some(PollMode::Active),
                        Some(m) if m.eq_ignore_ascii_case("passive") => Some(PollMode::Passive),
                        Some(other) => {
                            bail!("Invalid --poll-mode '{other}': expected 'active' or 'passive'")
                        }
                    };
                    Config::upsert_account(
                        cli.config.as_deref(),
                        UpsertAccountRequest {
                            provider: provider.clone(),
                            id: id.clone(),
                            label: label.clone(),
                            path: path.clone(),
                            binary: binary.clone(),
                            poll_mode: parsed_mode,
                            switch_command: switch_command.clone(),
                        },
                    )?
                }
                ConfigCommand::RemoveAccount { provider, id } => {
                    Config::remove_account(cli.config.as_deref(), provider, id)?
                }
            };
            Ok(Some(serde_json::to_string_pretty(&summary)?))
        }

        Some(Commands::Cached) => {
            let separator = Config::load(cli.config.as_deref())
                .map(|c| c.separator)
                .unwrap_or_else(|_| " | ".to_string());
            let report = runtime
                .cache_store
                .load_cached_report(&separator, runtime.now.timestamp());
            Ok(Some(format_report(&report, cli)?))
        }

        Some(Commands::Account {
            action:
                AccountCommand::Select {
                    provider_id,
                    account_id,
                },
        }) => {
            let provider_norm = provider_id.trim().to_ascii_lowercase();
            if !CANONICAL_PROVIDER_IDS.contains(&provider_norm.as_str()) {
                bail!(
                    "Unsupported provider '{}': expected one of {}",
                    provider_norm,
                    CANONICAL_PROVIDER_IDS.join(", ")
                );
            }

            let cfg = Config::load(cli.config.as_deref())?;
            let target_acct = account_id.trim();

            if !target_acct.is_empty() && target_acct != "default" {
                let (_, manual_accounts) = cfg.effective_provider_accounts(&provider_norm);
                let switch_cmd = manual_accounts
                    .iter()
                    .find(|m| {
                        m.id == target_acct
                            || format!("{provider_norm}:{}", m.id) == target_acct
                    })
                    .and_then(|m| m.switch_command.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty());

                if let Some(cmd) = switch_cmd {
                    let spec =
                        CommandSpec::new("/bin/sh", Duration::from_secs(cfg.timeout_seconds.max(1)))
                            .args(["-c", cmd]);
                    let output = proc_exec::run_command(&spec)
                        .with_context(|| format!("Failed to execute switch_command '{cmd}'"))?;
                    if !output.status.success() {
                        return Err(anyhow!(
                            "switch_command '{}' exited with status {:?}: {}",
                            cmd,
                            output.status.code(),
                            output.combined_text().trim()
                        ));
                    }
                }
            }

            let report = runtime.cache_store.select_active_account(
                &provider_norm,
                target_acct,
                &cfg.separator,
                runtime.now.timestamp(),
            )?;
            Ok(Some(format_report(&report, cli)?))
        }

        Some(Commands::IngestHook {
            target: IngestHookCommand::Claude { account, json },
        }) => {
            let Some(stdin_buf) = runtime.stdin_data else {
                return Ok(None);
            };
            if stdin_buf.trim().is_empty() {
                return Ok(None);
            }

            let cfg = Config::load(cli.config.as_deref()).unwrap_or_default();
            let report = claude::ingest_claude_statusline(
                &cfg,
                &runtime.cache_store,
                account.as_deref(),
                stdin_buf,
                runtime.now,
            )?;
            if *json || cli.pretty || cli.text {
                Ok(Some(format_report(&report, cli)?))
            } else {
                Ok(None)
            }
        }

        Some(Commands::Collect { force, provider }) => run_collect(
            cli,
            runtime,
            CollectOptions {
                force: *force,
                provider_filter: provider.clone(),
            },
        ),

        None => run_collect(cli, runtime, CollectOptions::default()),
    }
}

fn run_collect(
    cli: &Cli,
    runtime: &CliRuntime<'_>,
    opts: CollectOptions,
) -> Result<Option<String>> {
    let now_epoch = runtime.now.timestamp();
    let cfg = match Config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(err) => {
            // Emit valid StatusReport v2 JSON even when config fails to parse so the GNOME top bar renders the error cleanly.
            let fallback = StatusReport::config_error(format!("{:#}", err), now_epoch);
            return Ok(Some(format_report(&fallback, cli)?));
        }
    };

    for warning in cfg.unknown_provider_warnings() {
        eprintln!("Warning: {warning}");
    }

    let mut ctx = runtime
        .fetch_ctx_override
        .clone()
        .unwrap_or_else(|| FetchContext::from_config(&cfg));
    ctx.now = runtime.now;

    let report = ProviderRegistry::builtin().collect(&cfg, &runtime.cache_store, &opts, &ctx)?;
    Ok(Some(format_report(&report, cli)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_legacy_default_flags() {
        let cli = Cli::try_parse_from([
            "gnome-agent-status",
            "--config",
            "/tmp/custom.toml",
            "--pretty",
            "--text",
            "--color",
        ])
        .expect("parse legacy flags");

        assert_eq!(cli.config, Some(PathBuf::from("/tmp/custom.toml")));
        assert!(cli.pretty);
        assert!(cli.text);
        assert!(cli.color);
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_parse_collect_cached_and_account_select() {
        let collect_cli = Cli::try_parse_from([
            "gnome-agent-status",
            "--config",
            "/tmp/cfg.toml",
            "collect",
            "--force",
            "--provider",
            "claude",
            "--pretty",
        ])
        .expect("parse collect");

        assert_eq!(collect_cli.config, Some(PathBuf::from("/tmp/cfg.toml")));
        assert!(collect_cli.pretty);
        match collect_cli.command {
            Some(Commands::Collect { force, provider }) => {
                assert!(force);
                assert_eq!(provider.as_deref(), Some("claude"));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let cached_cli = Cli::try_parse_from(["gnome-agent-status", "cached", "--pretty"])
            .expect("parse cached");
        assert!(matches!(cached_cli.command, Some(Commands::Cached)));
        assert!(cached_cli.pretty);

        let acct_cli = Cli::try_parse_from([
            "gnome-agent-status",
            "account",
            "select",
            "claude",
            "claude:work",
        ])
        .expect("parse account select");
        match acct_cli.command {
            Some(Commands::Account {
                action:
                    AccountCommand::Select {
                        provider_id,
                        account_id,
                    },
            }) => {
                assert_eq!(provider_id, "claude");
                assert_eq!(account_id, "claude:work");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn test_parse_config_and_ingest_hook_subcommands() {
        let set_cli = Cli::try_parse_from([
            "gnome-agent-status",
            "config",
            "set",
            "--auto-discover",
            "false",
            "--timeout-seconds",
            "45",
        ])
        .expect("parse config set");
        match set_cli.command {
            Some(Commands::Config {
                action:
                    ConfigCommand::Set {
                        auto_discover,
                        timeout_seconds,
                    },
            }) => {
                assert_eq!(auto_discover, Some(false));
                assert_eq!(timeout_seconds, Some(45));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let upsert_cli = Cli::try_parse_from([
            "gnome-agent-status",
            "config",
            "upsert-account",
            "--provider",
            "claude",
            "--id",
            "work",
            "--label",
            "Work Claude",
            "--path",
            "~/.claude-work",
            "--poll-mode",
            "passive",
            "--switch-command",
            "claude-switch work",
        ])
        .expect("parse config upsert-account");
        match upsert_cli.command {
            Some(Commands::Config {
                action:
                    ConfigCommand::UpsertAccount {
                        provider,
                        id,
                        label,
                        path,
                        binary,
                        poll_mode,
                        switch_command,
                    },
            }) => {
                assert_eq!(provider, "claude");
                assert_eq!(id, "work");
                assert_eq!(label, "Work Claude");
                assert_eq!(path.as_deref(), Some("~/.claude-work"));
                assert!(binary.is_none());
                assert_eq!(poll_mode.as_deref(), Some("passive"));
                assert_eq!(switch_command.as_deref(), Some("claude-switch work"));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let remove_cli = Cli::try_parse_from([
            "gnome-agent-status",
            "config",
            "remove-account",
            "--provider",
            "claude",
            "--id",
            "work",
        ])
        .expect("parse config remove-account");
        assert!(matches!(
            remove_cli.command,
            Some(Commands::Config {
                action: ConfigCommand::RemoveAccount { .. }
            })
        ));

        let hook_cli = Cli::try_parse_from([
            "gnome-agent-status",
            "ingest-hook",
            "claude",
            "--account",
            "work",
        ])
        .expect("parse ingest-hook claude");
        match hook_cli.command {
            Some(Commands::IngestHook {
                target: IngestHookCommand::Claude { account, json },
            }) => {
                assert_eq!(account.as_deref(), Some("work"));
                assert!(!json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
