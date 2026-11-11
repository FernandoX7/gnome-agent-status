# gnome-agent-status

Track usage quotas, rolling session caps, reset countdowns, and multi-account status across **5 AI coding agent providers** from the GNOME Shell 45–50 top bar or any terminal.

## Highlights

- **5 Built-in Usage Providers**:
  - **Claude Code (`claude`)** — Weekly (`wk`), 5-hour session (`5h`), model-specific buckets (`Sonnet`, `Opus`, `Fable`, etc.), and `Extra usage` spend via passive `statusLine` hooks, Anthropic OAuth usage API, or `claude -p /usage` CLI fallback (for SSO / `setup-token` accounts).
  - **OpenAI Codex (`codex`)** — 5-hour session (`5h`), weekly (`wk`), code-review/additional limits, and credit balances via direct ChatGPT usage API, `codex app-server` JSON-RPC, or local session JSONL fallback.
  - **xAI Grok (`grok`)** — Unified rolling weekly compute pool (`wk`) and on-demand / prepaid credit balances via the Grok CLI billing endpoint (with legacy ACP stdio fallback).
  - **Google Antigravity (`agy`)** — All 4 model-group quotas (`Gemini · Weekly`, `Gemini · 5h`, `Claude & GPT · Weekly`, `Claude & GPT · 5h`), combining `<15ms` local `language_server` Connect-RPC (`GetUserStatus`) when the IDE is open with `agy -p /usage --output-format json`.
  - **Cursor (`cursor`)** — Monthly cycle buckets (`Cursor Models`, `Other Models`, `31d`) and plan spend/badges (`Pro`, `Ultra`) read safely from read-only SQLite `state.vscdb`, a JSON token file, or `CURSOR_ACCESS_TOKEN`.
- **Multi-Account Auto-Discovery & Custom Locations**: Automatically discovers and deduplicates local profiles (`~/.claude*`, `~/.codex*`, `~/.grok*`, Antigravity, Cursor) while letting you add custom directories, SQLite DBs, or token files via the Preferences file picker, CLI, or `config.toml`.
- **3 Top-Bar Panel Modes**:
  - **Minimal Icon** (`icon`, default) — Single monochrome symbolic glyph (`agent-status-symbolic.svg`) using native panel foreground below `80%`, shifting to subtle amber (`80–89%`) or coral-red (`>=90%` / auth error), with optional **Auto-Hide When Empty**.
  - **Compact Pills** (`compact-pills`) — Mini provider glyphs paired with tabular (`tnum`) primary percentages.
  - **Full Text** (`full-text`) — Classic inline text summary (`Claude: 89% | Codex: 19% | Grok: 10%`).
- **Expandable Multi-Page Popover (`352px`)**:
  - **Main Usage View**: Switch between **Detailed** (multi-bucket capsule bars, plan badges, live 60-second `Resets in …` countdowns) and **Compact** (single-row scannable bars) density modes, toggle `% Used` vs `% Remaining`, or trigger manual refresh (`↻`).
  - **Provider Drill-Down**: Click any provider to inspect its **Active Account Hero Card**, switch active accounts in place (with inline comparison mini-bars, passive `"Updates when <Provider> runs"` time-decay tracking, and optional `switch_command` hooks), or re-authenticate expired accounts via one-click **`[Sign in]`** terminal launch.
  - **Quick Options (`⚙`)**: Change Panel Mode, Theme Preset, Font Style (`System Tabular` / `Monospace`), Metric Mode, or Auto-Hide directly inside the popover.
- **8 Built-in Themes**:
  - **Midnight Slate** (`midnight-slate` — `#1e1f2f`, default)
  - **GNOME System** (`system` — follows native GNOME Shell dark/light style)
  - **Tokyo Night** (`tokyo-night` — `#1a1b26`)
  - **Catppuccin Mocha** (`catppuccin` — `#1e1e2e`)
  - **Gruvbox Dark** (`gruvbox` — `#282828`)
  - **Rosé Pine** (`rose-pine` — `#1f1d2e`)
  - **Nord** (`nord` — `#2e3440`)
  - **Everforest Dark** (`everforest` — `#2d353b`)
- **Zero-Daemon `<1ms` Reactive Cache Architecture**:
  - Pure-Rust synchronous binary (`ureq` + `rustls`, zero OpenSSL/C build dependencies) writes normalized `StatusReport` v2 JSON atomically (`flock` + temp-file `rename()`, mode `0600`, zero tokens or secrets) to `$XDG_CACHE_HOME/gnome-agent-status/state.json`.
  - The GNOME Shell extension hydrates directly from `state.json` via `Gio.File.load_contents_async` in `<1ms` on login/unlock without spawning a subprocess when TTL is fresh, reacts to CLI/hook updates in `<2ms` via `Gio.FileMonitor`, uses `0 MB` of resident background daemon RAM, and cleanly terminates any in-flight collector process group on screen lock.

---

## Installation

### Prerequisites

- **Rust toolchain** (`cargo` and `rustc`, stable)
- **GNOME Shell 45–50** *(required for the top-bar extension; the `gnome-agent-status` CLI runs standalone on Linux and macOS, including headless SSH servers)*
- **`glib-compile-schemas`** *(usually provided by `glib2`, `libglib2.0-bin`, or `glib2-devel`)*
- **`sqlite3` CLI** *(optional — used for non-locking read-only queries against Cursor's `state.vscdb`)*

### Build & Install

```bash
./install.sh
```

`./install.sh` performs four steps:

1. Compiles the release binary (`cargo build --release`) and installs it to `~/.local/bin/gnome-agent-status` *(ensure `~/.local/bin` is in your `PATH`, or configure a custom binary path in Preferences)*.
2. Initializes `~/.config/gnome-agent-status/config.toml` via `gnome-agent-status --init-config` if not already present.
3. Copies `extension/` (`popover/`, `icons/`, `schemas/`, `prefs.js`, `stylesheet.css`, `metadata.json`) to `~/.local/share/gnome-shell/extensions/agent-status@gnome-agent-status/` and compiles the GSettings schema with `glib-compile-schemas`.
4. Enables `agent-status@gnome-agent-status` via `gnome-extensions enable`.

> **Wayland note:** When installing the GNOME Shell extension for the first time on a Wayland session, log out and log back in so GNOME Shell picks up the newly installed extension.

### GNOME Extensions Preferences

Open the Libadwaita preferences window at any time from the popover (`Manage Accounts & Preferences…`) or from a terminal:

```bash
gnome-extensions prefs agent-status@gnome-agent-status
```

Preferences provides three pages:

1. **Appearance & Top Bar** — Panel mode (`Minimal Icon`, `Compact Pills`, `Full Text`), panel placement (`left`, `center`, `right` + index), popover density (`Detailed` / `Compact`), theme preset (all 8 themes), font style (`System Tabular` / `Monospace`), metric (`% Used` / `% Remaining`), and `Auto-Hide When Empty`.
2. **Refresh & Visibility** — Background refresh interval (default `300s`), per-provider visibility toggles, and optional custom `gnome-agent-status` binary override.
3. **Accounts & Manual Locations** — Toggle `auto_discover`, adjust collector timeout, and add, edit, or remove custom account directories/files with a native file picker.

---

## Provider Discovery & Multi-Account Setup

### Automatic Discovery (`auto_discover = true`)

By default, `gnome-agent-status` inspects standard local credential locations in **read-only** mode and deduplicates matching accounts across profile slots:

| Provider ID | Provider | Auto-Discovered Sources & Fallbacks | Default Poll TTL |
| :--- | :--- | :--- | :--- |
| `claude` | **Claude Code** | `~/.claude`, `~/.claude-*` (`.credentials.json` + `.claude.json`); falls back to `CLAUDE_CONFIG_DIR=<dir> claude -p /usage` for SSO / `setup-token` profiles | `300s` |
| `codex` | **OpenAI Codex** | `~/.codex`, `~/.codex-*` (`auth.json` with `cli_auth_credentials_store = "file"`), `codex app-server` (`account/rateLimits/read`), `$CODEX_HOME/sessions/**/*.jsonl`, and `~/.local/share/opencode/auth.json` | `300s` |
| `grok` | **xAI Grok** | `~/.grok`, `~/.grok-*`, `~/.grok-swap/accounts/*` (`auth.json` nested `https://auth.x.ai::<uuid>` or flat OAuth tokens), or `GROK_OAUTH_TOKEN` | `300s` |
| `agy` | **Google Antigravity** | Active local `language_server` Connect-RPC (`GetUserStatus` via `/proc` loopback discovery) + `agy -p /usage --output-format json`; dormant profiles use passive cached snapshots without touching OS keyring | `60s` (local RPC) / `300s` (`agy` CLI) |
| `cursor` | **Cursor** | `~/.config/Cursor/User/globalStorage/state.vscdb` (macOS: `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`), JSON token file, or `CURSOR_ACCESS_TOKEN` | `300s` |

### Adding Manual Account Locations

If an account lives in a custom directory, container volume, or non-standard path, you can register it in any of three ways:

#### 1. Preferences GUI File Picker

Run `gnome-extensions prefs agent-status@gnome-agent-status`, open **Accounts & Manual Locations**, and pick the profile folder (e.g. `~/.claude-work`, `~/.codex-work`, `~/.grok-work`) or credential/database file (e.g. a custom Cursor `state.vscdb` or JSON token file).

#### 2. CLI (`config upsert-account`)

```bash
# Add an isolated work Claude profile polled actively
gnome-agent-status config upsert-account \
  --provider claude \
  --id work \
  --label "Work Claude" \
  --path ~/.claude-work \
  --poll-mode active

# Add a custom Cursor state.vscdb path
gnome-agent-status config upsert-account \
  --provider cursor \
  --id custom \
  --label "Cursor Linux" \
  --path ~/.config/Cursor/User/globalStorage/state.vscdb

# Remove a manual account entry
gnome-agent-status config remove-account --provider claude --id work
```

#### 3. `config.toml` (`$XDG_CONFIG_HOME/gnome-agent-status/config.toml`)

See [`config.example.toml`](config.example.toml) for a complete template:

```toml
auto_discover = true
timeout_seconds = 30
separator = " | "

[providers.claude]
enabled = true

[[providers.claude.accounts]]
id = "work"
label = "Work Claude"
path = "~/.claude-work"
poll_mode = "active" # "active" | "passive"

[providers.codex]
enabled = true

[[providers.codex.accounts]]
id = "work"
label = "Work Codex"
path = "~/.codex-work"

[providers.grok]
enabled = true

[providers.agy]
enabled = true

[providers.cursor]
enabled = true
```

> **Backwards Compatibility:** Existing v0.1 `[[agents]]` entries (`name`, `type`, `path`, `binary`) continue to parse without changes and map automatically into `providers.<type>.accounts`.

### Optional: Claude Code `statusLine` Passive Hook (`0` API Calls)

Claude Code can pipe live `five_hour` and `seven_day` rate-limit JSON to a `statusLine` command after every turn. Feeding that stream into `gnome-agent-status ingest-hook claude` updates `state.json` and refreshes the GNOME top bar/popover in `<2ms` with **zero** background HTTP polling:

Add to `~/.claude/settings.json` (or `<CLAUDE_CONFIG_DIR>/settings.json`):

```json
{
  "statusLine": {
    "type": "command",
    "command": "gnome-agent-status ingest-hook claude"
  }
}
```

If you target a named profile or already use a custom status-line script, pass `--account <id>` and/or pipe via `tee`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "tee >(gnome-agent-status ingest-hook claude --account work >/dev/null) | ~/.claude/my-statusline.sh"
  }
}
```

---

## CLI Reference

All commands accept `--config <path>` to override `$XDG_CONFIG_HOME/gnome-agent-status/config.toml`.

### Default Collection & Terminal Output

```bash
# Collect (respecting per-account TTL & HTTP 429 backoff), update state.json, print JSON
gnome-agent-status [--pretty]

# Initialize default config.toml if missing
gnome-agent-status --init-config

# Print aligned terminal table (one row per provider/account)
gnome-agent-status --text

# Print aligned terminal table with ANSI threshold colors (<50% green, 50-79% yellow, >=80% red)
gnome-agent-status --text --color
```

### Subcommands

| Command | Description |
| :--- | :--- |
| `gnome-agent-status collect [--force] [--provider <id>] [--pretty]` | Refresh expired accounts, atomically write `state.json`, and print `StatusReport` v2 JSON. `--force` bypasses normal TTL while honoring a `30s` manual-refresh debounce and active HTTP `429` `retry_after` locks. |
| `gnome-agent-status cached [--pretty]` | Read `state.json` / `collector-cache.json`, apply live passive time-decay (`now >= resets_at_epoch => 0% used`), and print `StatusReport` v2 JSON in `<3ms` with zero network or subprocess I/O. |
| `gnome-agent-status account select <provider_id> <account_id>` | Pin `<account_id>` (or `"default"` to follow system default) as active for `<provider_id>`, execute optional `switch_command` if configured, update `state.json`, and print updated `StatusReport` v2 JSON. |
| `gnome-agent-status config show` | Print JSON summary of `config_path`, `auto_discover`, `timeout_seconds`, `separator`, and configured `manual_accounts`. |
| `gnome-agent-status config set [--auto-discover <true\|false>] [--timeout-seconds <u64>]` | Safely update top-level collector settings in `config.toml`. |
| `gnome-agent-status config upsert-account --provider <id> --id <slug> --label <label> [--path <path>] [--binary <bin>] [--poll-mode <active\|passive>] [--switch-command <cmd>]` | Create or update a `[[providers.<provider>.accounts]]` entry in `config.toml`, preserving existing sections, and print updated `config show` JSON. |
| `gnome-agent-status config remove-account --provider <id> --id <slug>` | Remove a manual account entry from `config.toml` and print updated `config show` JSON. |
| `gnome-agent-status ingest-hook claude [--account <id>]` | Read Claude Code `statusLine` JSON from `stdin` and atomically update passive `5h` / `wk` buckets in `collector-cache.json` and `state.json`. |

### Periodic Terminal & SSH Monitoring with `watch`

```bash
# Refresh terminal table every 120 seconds with ANSI colors
watch --color -n 120 gnome-agent-status --text --color

# Monitor a remote Linux workstation or devbox over SSH
ssh -t host 'watch --color -n 120 ~/.local/bin/gnome-agent-status --text --color'
```

### XDG Files & Security

- **Config**: `$XDG_CONFIG_HOME/gnome-agent-status/config.toml` *(fallback `~/.config/gnome-agent-status/config.toml`)*
- **UI State Cache**: `$XDG_CACHE_HOME/gnome-agent-status/state.json` *(mode `0600`, atomic temp-file + `rename()`; contains normalized percentages, timestamps, and labels only — **never** OAuth tokens, API keys, or CSRF secrets)*
- **Collector Metadata Cache**: `$XDG_CACHE_HOME/gnome-agent-status/collector-cache.json` *(mode `0600`; per-account fetch timestamps, HTTP `429` exponential backoff state, pinned active account IDs, and passive bucket snapshots)*

## License

This project uses the Apache License 2.0. Refer to the [LICENSE](LICENSE) file.
