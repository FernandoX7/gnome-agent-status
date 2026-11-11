//! Zero-secret, zero-PII diagnostic inspector (`gnome-agent-status doctor [--json]`).
//!
//! Inspects binary/OS metadata, `config.toml` syntax and `0600` permissions, XDG cache
//! directory (`0700`) and cache files (`0600`), GNOME Shell extension installation and compiled
//! GSettings schema presence, and per-provider discovery counts, source kinds, and credential
//! file permissions — strictly omitting and redacting all tokens, API keys, JWTs, and emails.

use crate::cli::CliRuntime;
use crate::config::{expand_tilde_path, home_dir, Config, ManualAccountConfig};
use crate::http::redact_secrets;
use crate::providers::{deduplicate_accounts, AccountDescriptor, FetchContext, ProviderRegistry};
use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Canonical GNOME Shell extension UUID.
pub const EXTENSION_UUID: &str = "agent-status@gnome-agent-status";

/// Known secret-bearing filenames inside provider profile directories.
const KNOWN_CREDENTIAL_FILENAMES: &[&str] = &[
    ".credentials.json",
    "credentials.json",
    "auth.json",
    "oauth_creds.json",
    "hosts.yml",
    "hosts.json",
    "apps.json",
    "api_key",
    "config.json",
    "state.vscdb",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub healthy: bool,
    pub binary: BinaryDiagnostics,
    pub config: ConfigDiagnostics,
    pub cache: CacheDiagnostics,
    pub gnome_extension: ExtensionDiagnostics,
    pub providers: Vec<ProviderDiagnostics>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryDiagnostics {
    pub name: String,
    pub version: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigDiagnostics {
    pub path: String,
    pub exists: bool,
    pub syntax_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub syntax_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_mode: Option<String>,
    pub expected_mode: String,
    pub mode_ok: bool,
    pub auto_discover: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_providers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathModeCheck {
    pub path: String,
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub expected_mode: String,
    pub mode_ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheDiagnostics {
    pub directory: PathModeCheck,
    pub state_json: PathModeCheck,
    pub collector_cache_json: PathModeCheck,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionDiagnostics {
    pub uuid: String,
    pub path: String,
    pub installed: bool,
    pub metadata_present: bool,
    pub extension_js_present: bool,
    pub compiled_schema_present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionCheckSummary {
    pub checked_paths: usize,
    pub missing_paths: usize,
    pub insecure_paths: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<String>,
    pub permissions_ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDiagnostics {
    pub id: String,
    pub display_name: String,
    pub enabled: bool,
    pub configured_accounts: usize,
    pub discovered_accounts: usize,
    pub effective_accounts: usize,
    pub source_kinds: Vec<String>,
    pub file_permissions: PermissionCheckSummary,
}

/// Redacts both credential tokens (`redact_secrets`) and email addresses while preserving
/// the literal GNOME Shell extension UUID (`agent-status@gnome-agent-status`).
pub fn sanitize_zero_pii(input: &str) -> String {
    static EMAIL_RE: OnceLock<Regex> = OnceLock::new();
    let email_re = EMAIL_RE.get_or_init(|| {
        Regex::new(r"(?i)\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b")
            .expect("valid email redaction regex")
    });

    let no_secrets = redact_secrets(input);
    email_re
        .replace_all(&no_secrets, "[REDACTED_EMAIL]")
        .into_owned()
}

fn abbreviate_safe_path(path: &Path, home: Option<&Path>) -> String {
    let rendered = if let Some(h) = home {
        if let Ok(rel) = path.strip_prefix(h) {
            if rel.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", rel.display())
            }
        } else {
            path.display().to_string()
        }
    } else {
        path.display().to_string()
    };
    sanitize_zero_pii(&rendered)
}

fn inspect_mode_octal(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        fs::symlink_metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

fn format_octal_mode(mode: u32) -> String {
    format!("{mode:04o}")
}

fn check_exact_path_mode(
    path: &Path,
    expected_octal: u32,
    home: Option<&Path>,
) -> PathModeCheck {
    let safe_path = abbreviate_safe_path(path, home);
    let expected_str = format_octal_mode(expected_octal);
    let exists = fs::symlink_metadata(path).is_ok();
    if !exists {
        return PathModeCheck {
            path: safe_path,
            exists: false,
            mode: None,
            expected_mode: expected_str,
            mode_ok: true,
        };
    }

    match inspect_mode_octal(path) {
        Some(actual) => PathModeCheck {
            path: safe_path,
            exists: true,
            mode: Some(format_octal_mode(actual)),
            expected_mode: expected_str,
            mode_ok: actual == expected_octal,
        },
        None => PathModeCheck {
            path: safe_path,
            exists: true,
            mode: None,
            expected_mode: expected_str,
            mode_ok: true,
        },
    }
}

fn resolve_doctor_config_path(
    config_arg: Option<&Path>,
    runtime: &CliRuntime<'_>,
) -> PathBuf {
    if let Some(explicit) = config_arg {
        return expand_tilde_path(explicit);
    }
    if let Some(ref ctx) = runtime.fetch_ctx_override {
        if let Some(ref xdg) = ctx.xdg_config_dir {
            return xdg.join("gnome-agent-status").join("config.toml");
        }
        if let Some(ref h) = ctx.home_dir {
            return h
                .join(".config")
                .join("gnome-agent-status")
                .join("config.toml");
        }
    }
    Config::default_config_path()
}

fn resolve_extension_dir(ctx: &FetchContext) -> PathBuf {
    let data_home = if let Some(xdg_data) = ctx.env_var("XDG_DATA_HOME") {
        PathBuf::from(xdg_data)
    } else if let Some(ref h) = ctx.home_dir {
        h.join(".local").join("share")
    } else {
        home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local")
            .join("share")
    };
    data_home.join("gnome-shell").join("extensions").join(EXTENSION_UUID)
}

fn classify_source_kind(desc: &AccountDescriptor, manual_accounts: &[ManualAccountConfig]) -> &'static str {
    let provider_prefix = format!("{}:", desc.provider_id);
    let slug = desc.id.strip_prefix(&provider_prefix).unwrap_or(&desc.id);
    let is_manual = manual_accounts
        .iter()
        .any(|m| m.id == slug || m.id == desc.id);

    if let Some(ref src) = desc.source_label {
        let lower = src.to_ascii_lowercase();
        if src.starts_with('$') || lower.contains("env") {
            return "env";
        }
        if lower.contains("local ide") || lower.contains("connect-rpc") || lower.contains("language_server") {
            return "local_rpc";
        }
        if lower.contains("keyring") || lower.contains("safe storage") {
            return "keyring";
        }
    }

    if let Some(ref p) = desc.path {
        if p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "vscdb" | "sqlite" | "db"))
        {
            return if is_manual {
                "manual_sqlite_db"
            } else {
                "sqlite_db"
            };
        }
        if is_manual {
            return "manual_path";
        }
        if p.is_dir() {
            return "local_dir";
        }
        return "local_file";
    }

    if is_manual {
        "manual_config"
    } else {
        "cli"
    }
}

fn inspect_provider_permissions(
    descriptors: &[AccountDescriptor],
    manual_accounts: &[ManualAccountConfig],
    home: Option<&Path>,
) -> PermissionCheckSummary {
    let mut candidate_paths: Vec<PathBuf> = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();

    for m in manual_accounts {
        if let Some(ref raw_path) = m.path {
            let trimmed = raw_path.trim();
            if !trimmed.is_empty() {
                let expanded = expand_tilde_with_home(Path::new(trimmed), home);
                if seen.insert(expanded.clone()) {
                    candidate_paths.push(expanded);
                }
            }
        }
    }

    for desc in descriptors {
        if let Some(ref p) = desc.path {
            if seen.insert(p.clone()) {
                candidate_paths.push(p.clone());
            }
        }
    }

    let mut checked_paths = 0usize;
    let mut missing_paths = 0usize;
    let mut insecure_paths = 0usize;
    let mut modes_set: BTreeSet<String> = BTreeSet::new();

    for path in candidate_paths {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            missing_paths += 1;
            continue;
        };

        if meta.is_dir() {
            let mut found_inner_file = false;
            for cred_name in KNOWN_CREDENTIAL_FILENAMES {
                let inner = path.join(cred_name);
                if fs::symlink_metadata(&inner).is_ok() {
                    found_inner_file = true;
                    checked_paths += 1;
                    if let Some(mode) = inspect_mode_octal(&inner) {
                        modes_set.insert(format_octal_mode(mode));
                        if (mode & 0o077) != 0 {
                            insecure_paths += 1;
                        }
                    }
                }
            }
            if !found_inner_file {
                checked_paths += 1;
                if let Some(mode) = inspect_mode_octal(&path) {
                    modes_set.insert(format_octal_mode(mode));
                    if (mode & 0o077) != 0 {
                        insecure_paths += 1;
                    }
                }
            }
        } else {
            checked_paths += 1;
            if let Some(mode) = inspect_mode_octal(&path) {
                modes_set.insert(format_octal_mode(mode));
                if (mode & 0o077) != 0 {
                    insecure_paths += 1;
                }
            }
        }
    }

    PermissionCheckSummary {
        checked_paths,
        missing_paths,
        insecure_paths,
        modes: modes_set.into_iter().collect(),
        permissions_ok: missing_paths == 0 && insecure_paths == 0,
    }
}

fn expand_tilde_with_home(path: &Path, home: Option<&Path>) -> PathBuf {
    if let Some(s) = path.to_str() {
        if s == "~" {
            if let Some(h) = home {
                return h.to_path_buf();
            }
        } else if let Some(rest) = s.strip_prefix("~/") {
            if let Some(h) = home {
                return h.join(rest);
            }
        }
    }
    expand_tilde_path(path)
}

/// Runs all zero-secret, zero-PII diagnostics and returns a structured [`DoctorReport`].
pub fn run_doctor(config_arg: Option<&Path>, runtime: &CliRuntime<'_>) -> DoctorReport {
    let mut warnings: Vec<String> = Vec::new();

    let resolved_cfg_path = resolve_doctor_config_path(config_arg, runtime);
    let cfg_exists = fs::symlink_metadata(&resolved_cfg_path).is_ok();

    let (cfg, syntax_valid, syntax_error) = if cfg_exists {
        match Config::load(Some(&resolved_cfg_path)) {
            Ok(parsed) => (parsed, true, None),
            Err(err) => {
                let clean_err = sanitize_zero_pii(&format!("{err:#}"));
                warnings.push(format!("Configuration syntax error: {clean_err}"));
                (Config::default(), false, Some(clean_err))
            }
        }
    } else {
        (Config::default(), true, None)
    };

    let mut ctx = runtime
        .fetch_ctx_override
        .clone()
        .unwrap_or_else(|| FetchContext::from_config(&cfg));
    ctx.now = runtime.now;
    let home = ctx.home_dir.as_deref();

    let cfg_mode_check = check_exact_path_mode(&resolved_cfg_path, 0o600, home);
    if cfg_exists && !cfg_mode_check.mode_ok {
        warnings.push(format!(
            "Config file {} has mode {} (expected 0600)",
            cfg_mode_check.path,
            cfg_mode_check.mode.as_deref().unwrap_or("unknown")
        ));
    }

    let unknown_providers: Vec<String> = cfg
        .unknown_provider_warnings()
        .into_iter()
        .map(|w| sanitize_zero_pii(&w))
        .collect();
    for w in &unknown_providers {
        warnings.push(w.clone());
    }

    let config_diag = ConfigDiagnostics {
        path: cfg_mode_check.path,
        exists: cfg_exists,
        syntax_valid,
        syntax_error,
        file_mode: cfg_mode_check.mode,
        expected_mode: "0600".to_string(),
        mode_ok: cfg_mode_check.mode_ok,
        auto_discover: cfg.auto_discover,
        unknown_providers,
    };

    // Cache directory (`0700`) and cache files (`0600`)
    let cache_dir_check = check_exact_path_mode(runtime.cache_store.cache_dir(), 0o700, home);
    let state_check = check_exact_path_mode(&runtime.cache_store.state_path(), 0o600, home);
    let collector_check =
        check_exact_path_mode(&runtime.cache_store.collector_cache_path(), 0o600, home);

    if cache_dir_check.exists && !cache_dir_check.mode_ok {
        warnings.push(format!(
            "Cache directory {} has mode {} (expected 0700)",
            cache_dir_check.path,
            cache_dir_check.mode.as_deref().unwrap_or("unknown")
        ));
    }
    if state_check.exists && !state_check.mode_ok {
        warnings.push(format!(
            "Cache state file {} has mode {} (expected 0600)",
            state_check.path,
            state_check.mode.as_deref().unwrap_or("unknown")
        ));
    }
    if collector_check.exists && !collector_check.mode_ok {
        warnings.push(format!(
            "Collector cache file {} has mode {} (expected 0600)",
            collector_check.path,
            collector_check.mode.as_deref().unwrap_or("unknown")
        ));
    }

    let cache_diag = CacheDiagnostics {
        directory: cache_dir_check,
        state_json: state_check,
        collector_cache_json: collector_check,
    };

    // GNOME Shell extension installation & compiled schema check
    let ext_dir = resolve_extension_dir(&ctx);
    let ext_installed = ext_dir.is_dir();
    let metadata_present = ext_dir.join("metadata.json").is_file();
    let extension_js_present = ext_dir.join("extension.js").is_file();
    let compiled_schema_present = ext_dir
        .join("schemas")
        .join("gschemas.compiled")
        .is_file();

    if ext_installed && !compiled_schema_present {
        warnings.push(format!(
            "GNOME Shell extension directory {} is missing schemas/gschemas.compiled",
            abbreviate_safe_path(&ext_dir, home)
        ));
    }

    let ext_diag = ExtensionDiagnostics {
        uuid: EXTENSION_UUID.to_string(),
        path: abbreviate_safe_path(&ext_dir, home),
        installed: ext_installed,
        metadata_present,
        extension_js_present,
        compiled_schema_present,
    };

    // Per-provider zero-secret, zero-email discovery summary
    let registry = ProviderRegistry::builtin();
    let mut provider_diags = Vec::with_capacity(registry.providers().len());

    for provider in registry.providers() {
        let meta = provider.meta();
        let (enabled, manual_accounts) = cfg.effective_provider_accounts(meta.id);
        let configured_accounts = manual_accounts.len();

        let (discovered_accounts, effective_accounts, source_kinds, file_permissions) = if enabled {
            let auto_only = if cfg.auto_discover {
                deduplicate_accounts(provider.discover_accounts(&[], true, &ctx))
            } else {
                Vec::new()
            };
            let combined = deduplicate_accounts(provider.discover_accounts(
                &manual_accounts,
                cfg.auto_discover,
                &ctx,
            ));

            let mut kinds_set: BTreeSet<String> = BTreeSet::new();
            for desc in &combined {
                kinds_set.insert(classify_source_kind(desc, &manual_accounts).to_string());
            }

            let perms = inspect_provider_permissions(&combined, &manual_accounts, home);
            if perms.insecure_paths > 0 {
                warnings.push(format!(
                    "Provider '{}' has {} credential path(s) with group/world permissions (expected 0600/0700)",
                    meta.id, perms.insecure_paths
                ));
            }
            if perms.missing_paths > 0 {
                warnings.push(format!(
                    "Provider '{}' has {} configured path(s) that do not exist",
                    meta.id, perms.missing_paths
                ));
            }

            (
                auto_only.len(),
                combined.len(),
                kinds_set.into_iter().collect(),
                perms,
            )
        } else {
            (
                0,
                0,
                Vec::new(),
                PermissionCheckSummary {
                    checked_paths: 0,
                    missing_paths: 0,
                    insecure_paths: 0,
                    modes: Vec::new(),
                    permissions_ok: true,
                },
            )
        };

        provider_diags.push(ProviderDiagnostics {
            id: meta.id.to_string(),
            display_name: meta.display_name.to_string(),
            enabled,
            configured_accounts,
            discovered_accounts,
            effective_accounts,
            source_kinds,
            file_permissions,
        });
    }

    let healthy = config_diag.syntax_valid
        && config_diag.mode_ok
        && cache_diag.directory.mode_ok
        && cache_diag.state_json.mode_ok
        && cache_diag.collector_cache_json.mode_ok
        && (!ext_diag.installed || ext_diag.compiled_schema_present)
        && provider_diags
            .iter()
            .all(|p| p.file_permissions.permissions_ok);

    DoctorReport {
        healthy,
        binary: BinaryDiagnostics {
            name: env!("CARGO_PKG_NAME").to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        },
        config: config_diag,
        cache: cache_diag,
        gnome_extension: ext_diag,
        providers: provider_diags,
        warnings,
    }
}

/// Formats [`DoctorReport`] as human-readable aligned text, applying final zero-PII sanitization.
pub fn format_doctor_text(report: &DoctorReport) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!(
        "{} {} ({}/{})",
        report.binary.name, report.binary.version, report.binary.os, report.binary.arch
    ));
    lines.push(format!(
        "Overall health: {}",
        if report.healthy { "OK" } else { "WARN" }
    ));
    lines.push(String::new());

    // Configuration section
    lines.push("Configuration:".to_string());
    if report.config.exists {
        let syntax_badge = if report.config.syntax_valid {
            "valid"
        } else {
            "INVALID"
        };
        let mode_str = report.config.file_mode.as_deref().unwrap_or("n/a");
        let mode_badge = if report.config.mode_ok {
            "ok"
        } else {
            "WARN: expected 0600"
        };
        lines.push(format!(
            "  path: {} (exists, syntax: {}, mode: {} [{}], auto_discover: {})",
            report.config.path, syntax_badge, mode_str, mode_badge, report.config.auto_discover
        ));
        if let Some(ref err) = report.config.syntax_error {
            lines.push(format!("  syntax_error: {err}"));
        }
    } else {
        lines.push(format!(
            "  path: {} (not created yet; using built-in defaults, auto_discover: {})",
            report.config.path, report.config.auto_discover
        ));
    }

    // Cache section
    lines.push(String::new());
    lines.push("Cache Store:".to_string());
    for (label, item) in [
        ("cache_dir", &report.cache.directory),
        ("state.json", &report.cache.state_json),
        ("collector-cache.json", &report.cache.collector_cache_json),
    ] {
        if item.exists {
            let mode_str = item.mode.as_deref().unwrap_or("n/a");
            let status = if item.mode_ok {
                "ok".to_string()
            } else {
                format!("WARN: expected {}", item.expected_mode)
            };
            lines.push(format!(
                "  {label:<22} {} (mode: {mode_str} [{status}])",
                item.path
            ));
        } else {
            lines.push(format!("  {label:<22} {} (not created yet)", item.path));
        }
    }

    // GNOME Shell Extension section
    lines.push(String::new());
    lines.push("GNOME Shell Extension:".to_string());
    if report.gnome_extension.installed {
        lines.push(format!(
            "  path: {} (installed, metadata.json: {}, extension.js: {}, gschemas.compiled: {})",
            report.gnome_extension.path,
            if report.gnome_extension.metadata_present {
                "yes"
            } else {
                "missing"
            },
            if report.gnome_extension.extension_js_present {
                "yes"
            } else {
                "missing"
            },
            if report.gnome_extension.compiled_schema_present {
                "yes"
            } else {
                "MISSING"
            }
        ));
    } else {
        lines.push(format!(
            "  path: {} (not installed — optional for CLI/Waybar/tmux)",
            report.gnome_extension.path
        ));
    }

    // Per-provider discovery summary
    lines.push(String::new());
    lines.push("Providers (zero-secret summary):".to_string());
    for p in &report.providers {
        if !p.enabled {
            lines.push(format!("  {:<12} ({:<12}) disabled", p.id, p.display_name));
            continue;
        }
        let sources = if p.source_kinds.is_empty() {
            "none".to_string()
        } else {
            p.source_kinds.join(",")
        };
        let perm_summary = if p.file_permissions.checked_paths == 0 && p.file_permissions.missing_paths == 0 {
            "n/a".to_string()
        } else if p.file_permissions.permissions_ok {
            format!("ok ({})", p.file_permissions.modes.join(","))
        } else {
            format!(
                "WARN (insecure={}, missing={}, modes=[{}])",
                p.file_permissions.insecure_paths,
                p.file_permissions.missing_paths,
                p.file_permissions.modes.join(",")
            )
        };
        lines.push(format!(
            "  {:<12} configured={} discovered={} active={} sources=[{}] perms={}",
            p.id,
            p.configured_accounts,
            p.discovered_accounts,
            p.effective_accounts,
            sources,
            perm_summary
        ));
    }

    if !report.warnings.is_empty() {
        lines.push(String::new());
        lines.push("Warnings:".to_string());
        for w in &report.warnings {
            lines.push(format!("  - {w}"));
        }
    }

    sanitize_zero_pii(&lines.join("\n"))
}

/// Serializes [`DoctorReport`] as pretty JSON (or compact when `pretty == false`) with zero-PII enforcement.
pub fn format_doctor_json(report: &DoctorReport, pretty: bool) -> Result<String> {
    let raw = if pretty {
        serde_json::to_string_pretty(report)?
    } else {
        serde_json::to_string(report)?
    };
    Ok(sanitize_zero_pii(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheStore;
    use crate::http::{EndpointOverrides, HttpClient};
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::time::Duration;

    #[test]
    fn test_sanitize_zero_pii_redacts_secrets_and_emails_while_keeping_extension_uuid() {
        let sample = format!(
            "path=/home/alice@secret-corp.io/.local/share/gnome-shell/extensions/{EXTENSION_UUID} \
             key=sk-or-v1-abcdef0123456789abcdef0123456789 \
             ds=sk-9876543210abcdef9876543210abcdef \
             user=bob.builder+dev@example.org"
        );
        let clean = sanitize_zero_pii(&sample);
        assert!(clean.contains(EXTENSION_UUID));
        assert!(!clean.contains("alice@secret-corp.io"));
        assert!(!clean.contains("bob.builder+dev@example.org"));
        assert!(!clean.contains("sk-or-v1-"));
        assert!(!clean.contains("sk-9876543210"));
        assert!(clean.contains("[REDACTED_EMAIL]"));
        assert!(clean.contains("[REDACTED]"));
    }

    #[test]
    fn test_doctor_checks_modes_schema_providers_and_leaks_zero_secrets_or_emails() {
        let tmp = std::env::temp_dir().join(format!(
            "gas-doctor-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let home = tmp.join("home");
        let xdg_cfg = home.join(".config");
        let xdg_data = home.join(".local/share");
        let cache_dir = home.join(".cache/gnome-agent-status");

        fs::create_dir_all(&cache_dir).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&cache_dir, fs::Permissions::from_mode(0o700)).unwrap();

        let cache_store = CacheStore::new(cache_dir.clone());
        let state_path = cache_store.state_path();
        fs::write(&state_path, "{}").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&state_path, fs::Permissions::from_mode(0o600)).unwrap();

        // Collector cache intentionally given 0644 so doctor flags it!
        let coll_path = cache_store.collector_cache_path();
        fs::write(&coll_path, "{}").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&coll_path, fs::Permissions::from_mode(0o644)).unwrap();

        // Config file with 0644 mode and seeded email in manual label to ensure label/email is NEVER printed!
        let cfg_dir = xdg_cfg.join("gnome-agent-status");
        fs::create_dir_all(&cfg_dir).unwrap();
        let cfg_path = cfg_dir.join("config.toml");
        let openrouter_key_file = xdg_cfg.join("openrouter/api_key");
        fs::create_dir_all(openrouter_key_file.parent().unwrap()).unwrap();
        fs::write(
            &openrouter_key_file,
            "sk-or-v1-supersecretkeyvalue000000112233445566778899\n",
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&openrouter_key_file, fs::Permissions::from_mode(0o644)).unwrap();

        let claude_dir = home.join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        let claude_creds = claude_dir.join(".credentials.json");
        fs::write(
            &claude_creds,
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-mocktest000000000000000000","email":"ceo@private-startup.ai"}}"#,
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&claude_creds, fs::Permissions::from_mode(0o600)).unwrap();

        fs::write(
            &cfg_path,
            r#"
auto_discover = true

[[providers.openrouter.accounts]]
id = "work"
label = "founder@stealth-company.io"
path = "~/.config/openrouter/api_key"
"#,
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&cfg_path, fs::Permissions::from_mode(0o644)).unwrap();

        // GNOME Shell extension installed WITH compiled schema
        let ext_dir = xdg_data.join("gnome-shell/extensions").join(EXTENSION_UUID);
        fs::create_dir_all(ext_dir.join("schemas")).unwrap();
        fs::write(ext_dir.join("metadata.json"), "{}").unwrap();
        fs::write(ext_dir.join("extension.js"), "// ext").unwrap();
        fs::write(ext_dir.join("schemas/gschemas.compiled"), b"GVariant").unwrap();

        let mut env_map = BTreeMap::new();
        env_map.insert(
            "DEEPSEEK_API_KEY".to_string(),
            "sk-deepseeksecretvalue1234567890abcdef".to_string(),
        );
        env_map.insert(
            "XDG_DATA_HOME".to_string(),
            xdg_data.to_string_lossy().to_string(),
        );

        let ctx = FetchContext {
            http: HttpClient::new(Duration::from_secs(2)),
            timeout: Duration::from_secs(2),
            now: Utc::now(),
            home_dir: Some(home.clone()),
            xdg_config_dir: Some(xdg_cfg.clone()),
            proc_root: tmp.join("empty-proc"),
            endpoints: EndpointOverrides::default(),
            env_overrides: Some(env_map),
        };
        let runtime = CliRuntime {
            cache_store,
            fetch_ctx_override: Some(ctx),
            now: Utc::now(),
            stdin_data: None,
        };

        let report = run_doctor(Some(&cfg_path), &runtime);
        assert!(!report.healthy, "0644 files should mark healthy=false");
        assert!(report.config.exists);
        assert!(report.config.syntax_valid);
        assert!(!report.config.mode_ok);
        assert!(report.cache.directory.mode_ok);
        assert!(report.cache.state_json.mode_ok);
        assert!(!report.cache.collector_cache_json.mode_ok);
        assert!(report.gnome_extension.installed);
        assert!(report.gnome_extension.compiled_schema_present);
        assert_eq!(report.providers.len(), 10);

        let claude_p = report.providers.iter().find(|p| p.id == "claude").unwrap();
        assert_eq!(claude_p.effective_accounts, 1);
        assert!(claude_p.file_permissions.permissions_ok);

        let or_p = report
            .providers
            .iter()
            .find(|p| p.id == "openrouter")
            .unwrap();
        assert_eq!(or_p.configured_accounts, 1);
        assert_eq!(or_p.effective_accounts, 1);
        assert!(!or_p.file_permissions.permissions_ok);
        assert_eq!(or_p.file_permissions.insecure_paths, 1);

        let ds_p = report
            .providers
            .iter()
            .find(|p| p.id == "deepseek")
            .unwrap();
        assert_eq!(ds_p.discovered_accounts, 1);
        assert_eq!(ds_p.source_kinds, vec!["env".to_string()]);

        let text_out = format_doctor_text(&report);
        let json_out = format_doctor_json(&report, true).unwrap();

        for rendered in [&text_out, &json_out] {
            assert!(!rendered.contains("sk-or-v1-"));
            assert!(!rendered.contains("supersecretkeyvalue"));
            assert!(!rendered.contains("sk-ant-oat01-"));
            assert!(!rendered.contains("deepseeksecretvalue"));
            assert!(!rendered.contains("ceo@private-startup.ai"));
            assert!(!rendered.contains("founder@stealth-company.io"));
            assert!(rendered.contains(EXTENSION_UUID));
        }

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_doctor_malformed_config_redacts_inline_secrets_and_emails() {
        let tmp = std::env::temp_dir().join(format!(
            "gas-doctor-badcfg-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        fs::create_dir_all(&tmp).unwrap();
        let bad_cfg = tmp.join("config.toml");
        fs::write(
            &bad_cfg,
            "broken_toml = [unclosed sk-or-v1-000000secretleak998877665544332211 leak@private-domain.org\n",
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&bad_cfg, fs::Permissions::from_mode(0o600)).unwrap();

        let runtime = CliRuntime {
            cache_store: CacheStore::new(tmp.join("cache")),
            fetch_ctx_override: Some(FetchContext {
                http: HttpClient::new(Duration::from_secs(1)),
                timeout: Duration::from_secs(1),
                now: Utc::now(),
                home_dir: Some(tmp.clone()),
                xdg_config_dir: Some(tmp.join(".config")),
                proc_root: tmp.join("proc"),
                endpoints: EndpointOverrides::default(),
                env_overrides: Some(BTreeMap::new()),
            }),
            now: Utc::now(),
            stdin_data: None,
        };

        let report = run_doctor(Some(&bad_cfg), &runtime);
        assert!(!report.healthy);
        assert!(!report.config.syntax_valid);
        let err = report.config.syntax_error.as_deref().unwrap_or("");
        assert!(!err.contains("secretleak998877665544332211"));
        assert!(!err.contains("leak@private-domain.org"));

        let _ = fs::remove_dir_all(&tmp);
    }
}
