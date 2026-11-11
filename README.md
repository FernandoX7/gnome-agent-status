# gnome-agent-status

Track usage quotas, rolling session caps, burn-rate pacing, reset countdowns, and multi-account status across **7 AI coding agent providers** from the GNOME Shell 45–50 top bar, Waybar, `tmux`, or any terminal.

<table>
  <tr>
    <td align="center" valign="top" width="52%">
      <img src="docs/images/popover-detailed.png" alt="Detailed Usage Popover and 16x16 Symbolic Robot Indicator" width="352" />
      <br />
      <sub><b>Detailed 352px Popover &amp; 16×16 Symbolic Robot Icon</b> — Full-width 1-line-per-bucket bars, official brand marks, plan badges, and live countdowns.</sub>
    </td>
    <td align="center" valign="top" width="48%">
      <img src="docs/images/context-menu-pills.png" alt="Compact Provider Pills and Right-Click Context Menu" width="320" />
      <br />
      <sub><b>Compact Top-Bar Pills &amp; Right-Click Menu</b> — Quick access to <i>Refresh Usage</i>, <i>Settings…</i>, and <i>Quit Agent Status</i>.</sub>
      <br /><br />
      <img src="docs/images/brand-icons.png" alt="Symbolic Robot Status Icon and Official Provider Avatars" width="340" />
      <br />
      <sub><b>Pixel-Aligned Vector Iconography</b> — 16×16 symbolic robot with calm severity states alongside crisp 24×24 brand avatars.</sub>
    </td>
  </tr>
</table>

---

## Highlights

- **7 Built-in AI Coding Providers**:
  - **Claude Code (`claude`)** — Weekly (`Weekly`), 5-hour session (`5-Hour`), model-specific buckets (`Sonnet`, `Opus`, `Fable`, etc.), and `Extra Usage` spend via passive `statusLine` hooks (`ingest-hook claude`), Anthropic OAuth usage API, or `claude -p /usage` CLI fallback (for SSO / `setup-token` profiles).
  - **OpenAI Codex (`codex`)** — 5-hour session (`5-Hour`), weekly (`Weekly`), additional limits, and credit balances via direct ChatGPT usage API, `codex app-server` JSON-RPC (`account/rateLimits/read`), or offline session rollout JSONL / zstd-compressed `.jsonl.zst` (`$CODEX_HOME/sessions/**/rollout-*.jsonl[.zst]`) fallback.
  - **xAI Grok (`grok`)** — Unified rolling weekly compute pool (`Weekly`) and on-demand / prepaid credit balances via the Grok CLI billing endpoint (with legacy ACP stdio fallback).
  - **Google Antigravity (`agy`)** — All 4 model-family quotas (`Gemini · Weekly`, `Gemini · 5-Hour`, `Claude & GPT · Weekly`, `Claude & GPT · 5-Hour`), combining Antigravity 2.0 Linux `daemon/hub_<port>.json` & `/proc` local `language_server` Connect-RPC (`GetUserStatus` in `<2ms`) when the IDE is open with `agy -p /usage --output-format json`.
  - **Cursor (`cursor`)** — Monthly cycle buckets (`Cursor Models`, `Other Models`, `Monthly`) and plan spend/badges (`Pro`, `Ultra`) via embedded `rusqlite` (`immutable=1` non-locking read-only access — zero external `sqlite3` CLI dependency!) plus Electron `safeStorage` (`v10` and Linux `v11`) in-memory decryption, JSON token file, or `CURSOR_ACCESS_TOKEN`.
  - **GitHub Copilot (`copilot`)** — Monthly `Premium Requests` (`31d`) plus metered `Chat` / `Completions` quotas and plan badges (`Free`, `Pro`, `Pro+`, `Business`, `Enterprise`) via `copilot_internal/user`, reading `GITHUB_COPILOT_TOKEN`, `~/.config/github-copilot/{hosts,apps}.json`, or `~/.config/gh/hosts.yml` (automatically omitting noise when `gh` is used without an active Copilot subscription).
  - **Gemini CLI (`gemini`)** — Daily model-group quotas (`Gemini Pro · Daily`, `Gemini Flash · Daily`) via Cloud Code `v1internal:retrieveUserQuota`, inspecting `~/.gemini/oauth_creds.json`, `~/.gemini-*/oauth_creds.json`, or `GEMINI_CLI_OAUTH_TOKEN` strictly read-only.
- **Calm, Accessible GNOME Shell 45–50 Experience**:
  - **Cute `16x16` Symbolic Robot Indicator** (`agent-status-symbolic.svg`) in Minimal Icon mode — blends into native panel foreground below `80%`, shifting calmly to amber (`80–89%`) or coral-red (`>=90%` / auth error), alongside **Compact Pills** (`compact-pills`) and **Full Text** (`full-text`) modes.
  - **Right-Click Top-Bar Menu** — Secondary-click the panel indicator at any time for **Refresh Usage**, **Settings…**, or **Quit Agent Status** (cleanly disables the extension until re-enabled).
  - **Full-Width 1-Line-Per-Bucket Layout & Hover Tooltips** — Every quota bucket renders on its own dedicated line inside the `352px` popover (zero label truncation in both `System Tabular` and `Monospace`), paired with accessible opacity-fade hover and keyboard-focus tooltips that honor `org.gnome.desktop.interface` `enable-animations`.
  - **Burn-Rate / Pace Indicator** — Provider drill-down hero cards compute live sustainable quota pacing (`Pace: Comfortable`, `Pace: On track · ~88% by reset`, `Pace: 1.9× sustainable · hits 100% before reset`) from window duration and remaining reset time.
  - **Smart Quota & Reset Desktop Notifications** — Deduplicated per-window alerts when an active provider crosses `80%` (*Warning*), `90%` (*Critical*), or resets back to fresh capacity (*Quota Reset*), seeded silently on initial session hydration so login/unlock never spams.
  - **Custom Provider Ordering** — Reorder all 7 providers (`provider-order`) with Move Up / Move Down / Reset Order controls in Libadwaita Preferences, updating top-bar pills, text summaries, and popover cards immediately.
  - **Suspend/Resume, Network & Credential-File Auto-Refresh** — Automatically refreshes after system wake (`org.freedesktop.login1.Manager` `PrepareForSleep`), network reconnect (`Gio.NetworkMonitor`), or local credential sign-in (`Gio.FileMonitor`, updating the popover within `~1.5s` of completing CLI login).
- **8 Built-in Color Themes**:
  - **Midnight Slate** (`midnight-slate` — `#1e1f2f`, default), **GNOME System** (`system` — follows native dark/light shell style), **Tokyo Night** (`tokyo-night`), **Catppuccin Mocha** (`catppuccin`), **Gruvbox Dark** (`gruvbox`), **Rosé Pine** (`rose-pine`), **Nord** (`nord`), and **Everforest Dark** (`everforest`).
- **Standalone CLI, Waybar & `tmux` Integration**:
  - Run `gnome-agent-status` on any Linux or macOS machine (including non-GNOME desktops and headless SSH servers) with JSON, aligned ANSI tables (`--text --color`), Waybar JSON (`--waybar`), and `tmux` status segments (`--tmux`).

---

## Installation & Lifecycle

### Prerequisites

- **Rust toolchain** (`cargo` and `rustc`, stable)
- **GNOME Shell 45–50** *(required for the top-bar extension; the `gnome-agent-status` CLI runs standalone on Linux and macOS, including Waybar/`tmux`/SSH environments)*
- **`glib-compile-schemas`** *(provided by `libglib2.0-bin`, `glib2`, or `glib2-devel`)*

*(SQLite is compiled directly into the binary via bundled `rusqlite`, so no external `sqlite3` or OpenSSL system libraries are needed.)*

### Build & Install

```bash
./install.sh
```

`./install.sh` performs four steps:

1. Compiles the optimized release binary (`cargo build --release`) and installs it to `~/.local/bin/gnome-agent-status` *(ensure `~/.local/bin` is on `PATH`, or set a custom binary path in Preferences)*.
2. Initializes `~/.config/gnome-agent-status/config.toml` via `gnome-agent-status --init-config` if not already present.
3. Installs `extension/` (`popover/`, `icons/`, `schemas/`, `prefs.js`, `stylesheet.css`, `metadata.json`) into `~/.local/share/gnome-shell/extensions/agent-status@gnome-agent-status/` and compiles the GSettings schema with `glib-compile-schemas --strict`.
4. Enables `agent-status@gnome-agent-status` in GNOME Shell (with fail-closed GSettings pre-enabling for first-time Wayland installs).

> **Wayland note:** When installing a GNOME Shell extension for the first time on a Wayland session, log out and log back in once so GNOME Shell discovers the new extension directory.

### Uninstall

```bash
# Remove extension files, CLI binary, and enabled-extensions entry (preserving config & cache)
./uninstall.sh

# Remove everything including ~/.config/gnome-agent-status and ~/.cache/gnome-agent-status
./uninstall.sh --purge
```

`./uninstall.sh` disables `agent-status@gnome-agent-status` if running, removes only `'agent-status@gnome-agent-status'` from `org.gnome.shell` `enabled-extensions` / `disabled-extensions` (preserving all other user extensions verbatim), and deletes the extension directory and `~/.local/bin/gnome-agent-status`.

### Preferences Window (Libadwaita)

Open Preferences from the popover (**Manage Accounts & Preferences…**), from the top-bar right-click menu (**Settings…**), or from a terminal:

```bash
gnome-extensions prefs agent-status@gnome-agent-status
```

Preferences organizes settings across three searchable pages:

1. **Appearance & Top Bar** — Panel mode (`Minimal Icon`, `Compact Pills`, `Full Text`), panel box (`Left`, `Center`, `Right`) & index, `Auto-Hide When Empty`, theme preset (8 themes), popover density (`Detailed` / `Compact`), typography (`System Tabular` / `Monospace`), and metric (`% Used` / `% Remaining`).
2. **Refresh & Visibility** — Background refresh interval (`300s` default), `auto_discover` toggle, collector timeout, optional custom binary path, **Desktop Notifications** (`Warning Threshold (80%)`, `Critical Threshold (90%)`, `Quota Reset Notification`), **Provider Display Order** (Move Up / Move Down / Reset Order across all 7 providers), and **Visible Providers** switches.
3. **Accounts & Manual Locations** — View, add, edit, or remove custom account directories, credential files, or SQLite databases in `config.toml` using native GTK4 file/folder pickers.

---

## Provider Discovery & Multi-Account Setup

### Automatic Discovery (`auto_discover = true`)

By default, `gnome-agent-status` inspects standard local credential locations in **strictly read-only** mode and deduplicates matching accounts across profile slots:

| Provider ID | Provider | Auto-Discovered Sources & Fallbacks | Default Poll TTL |
| :--- | :--- | :--- | :--- |
| `claude` | **Claude Code** | `~/.claude`, `~/.claude-*` (`.credentials.json` + `.claude.json`); passive `ingest-hook claude`; falls back to `CLAUDE_CONFIG_DIR=<dir> claude -p /usage` for SSO / `setup-token` profiles | `300s` |
| `codex` | **OpenAI Codex** | `~/.codex`, `~/.codex-*` (`auth.json`), `codex app-server` (`account/rateLimits/read`), `$CODEX_HOME/sessions/**/rollout-*.jsonl` & `.jsonl.zst` zstd rollouts, and `~/.local/share/opencode/auth.json` | `300s` |
| `grok` | **xAI Grok** | `~/.grok`, `~/.grok-*`, `~/.grok-swap/accounts/*` (`auth.json` nested `https://auth.x.ai::<uuid>` or flat OAuth tokens), or `GROK_OAUTH_TOKEN` | `300s` |
| `agy` | **Google Antigravity** | Antigravity 2.0 Linux `~/.gemini/antigravity/daemon/hub_<port>.json` & `/proc` local `language_server` Connect-RPC (`GetUserStatus` in `<2ms`) + `agy -p /usage --output-format json`; dormant profiles use passive cached snapshots without touching OS keyring | `60s` (local RPC) / `300s` (`agy` CLI) |
| `cursor` | **Cursor** | Embedded `rusqlite` (`mode=ro&immutable=1` + `0600` temp WAL fallback) on `~/.config/Cursor/User/globalStorage/state.vscdb` (macOS: `~/Library/Application Support/Cursor/...`), Electron `safeStorage` (`v10` / Linux `v11`), JSON token file, or `CURSOR_ACCESS_TOKEN` | `300s` |
| `copilot` | **GitHub Copilot** | `GITHUB_COPILOT_TOKEN`, `~/.config/github-copilot/hosts.json` / `apps.json`, or `~/.config/gh/hosts.yml` (with read-only `gh auth token -h github.com` keyring fallback; silently skipped if `gh` account has no Copilot plan) | `300s` |
| `gemini` | **Gemini CLI** | `~/.gemini/oauth_creds.json`, `~/.gemini-*/oauth_creds.json` (+ `google_accounts.json` email label), or `GEMINI_CLI_OAUTH_TOKEN` querying `v1internal:retrieveUserQuota`; degrades to passive cache when expired without rotating refresh tokens | `300s` |

### Adding Manual Account Locations

Register non-standard directories, container volumes, or secondary profiles via Preferences (**Accounts & Manual Locations**), the CLI, or `config.toml`:

```bash
# Add an isolated work Claude profile polled actively
gnome-agent-status config upsert-account \
  --provider claude \
  --id work \
  --label "Work Claude" \
  --path ~/.claude-work \
  --poll-mode active

# Add a custom GitHub Copilot token/hosts file
gnome-agent-status config upsert-account \
  --provider copilot \
  --id enterprise \
  --label "Work Copilot" \
  --path ~/.config/github-copilot/hosts.json

# Add a secondary Gemini CLI profile directory
gnome-agent-status config upsert-account \
  --provider gemini \
  --id work \
  --label "Work Gemini" \
  --path ~/.gemini-work

# Remove a manual account entry
gnome-agent-status config remove-account --provider claude --id work
```

Example [`config.toml`](config.example.toml) (`$XDG_CONFIG_HOME/gnome-agent-status/config.toml`):

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

[providers.grok]
enabled = true

[providers.agy]
enabled = true

[providers.cursor]
enabled = true

[providers.copilot]
enabled = true

[providers.gemini]
enabled = true
```

> **Backwards Compatibility:** Existing v0.1 `[[agents]]` entries (`name`, `type`, `path`, `binary`) continue to parse seamlessly and map into `providers.<type>.accounts`.

### Optional: Claude Code `statusLine` Passive Hook (`0` API Calls)

Claude Code can pipe live `five_hour` and `seven_day` rate-limit JSON to a `statusLine` command after every turn. Feeding that stream into `gnome-agent-status ingest-hook claude` updates `state.json` and refreshes the GNOME top bar/popover in `<2ms` with **zero** background HTTP polling:

```json
{
  "statusLine": {
    "type": "command",
    "command": "gnome-agent-status ingest-hook claude"
  }
}
```

If you target a named profile or chain into an existing status-line script:

```json
{
  "statusLine": {
    "type": "command",
    "command": "tee >(gnome-agent-status ingest-hook claude --account work >/dev/null) | ~/.claude/my-statusline.sh"
  }
}
```

---

## Waybar & `tmux` Status Bar Integration

Both top-level collection (`gnome-agent-status`) and zero-network cache reads (`gnome-agent-status cached`, `<2ms`) support `--waybar` and `--tmux`.

### Waybar (`--waybar`)

`--waybar` emits single-line Waybar custom-module JSON (`{"text":"…","tooltip":"…","class":"normal|warning|critical|error","percentage":<u8>}`) with an aligned multi-provider breakdown inside the hover tooltip.

Add to `~/.config/waybar/config.jsonc`:

```jsonc
"custom/agent-status": {
  "exec": "~/.local/bin/gnome-agent-status --waybar",
  "return-type": "json",
  "interval": 300,
  "on-click": "~/.local/bin/gnome-agent-status collect --force >/dev/null",
  "tooltip": true
}
```

And style severity classes in `~/.config/waybar/style.css`:

```css
#custom-agent-status {
  padding: 0 10px;
}
#custom-agent-status.warning {
  color: #e0af68;
}
#custom-agent-status.critical,
#custom-agent-status.error {
  color: #ff4443;
}
```

### `tmux` (`--tmux`)

`--tmux` emits a compact `#[fg=…]`-styled status segment ideal for `status-right` (use `cached --tmux` for `<2ms` zero-blocking redraws alongside a periodic background refresh, or direct `--tmux` with a `300s` `status-interval`).

Add to `~/.tmux.conf`:

```tmux
set -g status-interval 60
set -g status-right-length 120
set -g status-right "#(~/.local/bin/gnome-agent-status --tmux) #[fg=brightblack]|#[default] %H:%M"
```

---

## CLI Reference

All commands accept `--config <path>` to override `$XDG_CONFIG_HOME/gnome-agent-status/config.toml`.

```bash
# Collect (respecting per-account TTL & HTTP 429 backoff), update state.json, print JSON
gnome-agent-status [--pretty]

# Print aligned terminal table (optionally with ANSI severity colors)
gnome-agent-status --text [--color]

# Print Waybar JSON or tmux status-right segment
gnome-agent-status --waybar
gnome-agent-status --tmux

# Read cached snapshot with live reset time-decay in <2ms (zero network/subprocess I/O)
gnome-agent-status cached [--pretty | --waybar | --tmux]
```

| Subcommand | Description |
| :--- | :--- |
| `gnome-agent-status collect [--force] [--provider <id>] [--pretty]` | Refresh expired accounts, atomically write `state.json`, and print `StatusReport` v2 JSON. `--force` bypasses normal TTL while honoring a `30s` manual-refresh debounce and active HTTP `429` `retry_after` locks. |
| `gnome-agent-status cached [--pretty \| --waybar \| --tmux]` | Read `state.json` / `collector-cache.json`, apply live passive time-decay (`now >= resets_at_epoch => 0% used`), and print output in `<2ms` with zero network or subprocess I/O. |
| `gnome-agent-status account select <provider_id> <account_id>` | Pin `<account_id>` (or `"default"`) as active for `<provider_id>`, execute optional `switch_command` if configured, update `state.json`, and print updated `StatusReport` v2 JSON. |
| `gnome-agent-status config show` | Print JSON summary of `config_path`, `auto_discover`, `timeout_seconds`, `separator`, and configured `manual_accounts`. |
| `gnome-agent-status config set [--auto-discover <true\|false>] [--timeout-seconds <u64>]` | Safely update top-level collector settings in `config.toml`. |
| `gnome-agent-status config upsert-account --provider <id> --id <slug> --label <label> [...]` | Create or update a `[[providers.<provider>.accounts]]` entry in `config.toml`, preserving comments and formatting via `toml_edit`. |
| `gnome-agent-status config remove-account --provider <id> --id <slug>` | Remove a manual account entry from `config.toml` and print updated `config show` JSON. |
| `gnome-agent-status ingest-hook claude [--account <id>]` | Read Claude Code `statusLine` JSON from `stdin` and atomically update passive `5-Hour` / `Weekly` buckets in `collector-cache.json` and `state.json`. |

---

## Security & Privacy Architecture

- **Zero Secrets on Disk or Wire Logs**: `$XDG_CACHE_HOME/gnome-agent-status/state.json` and `collector-cache.json` store only normalized percentages, reset epochs, plan labels, and sanitized status notes — **never** OAuth access/refresh tokens, API keys, cookies, or `--csrf_token` values.
- **Strict `0600` + `O_NOFOLLOW` Atomic Writes**: Config and cache directories are enforced at mode `0700`, and all state/config writes (`state.json`, `collector-cache.json`, `config.toml`, and temporary SQLite WAL snapshots) use `create_new(true)` + `O_NOFOLLOW` unique PID+nonce temp files with mode `0600`, `flock` serialization, and atomic `rename()`, preventing symlink and TOCTOU races.
- **Automatic Credential Redaction**: Upstream HTTP and CLI error messages pass through a token-redaction sanitizer stripping `Bearer` headers, JWTs (`eyJ…`), Anthropic keys (`sk-ant-…`), GitHub tokens (`gho_…`, `ghu_…`, `github_pat_…`), and Google keys/tokens (`AIza…`, `ya29.…`) before any diagnostic string can reach `state.json` or `stderr`.
- **Bounded HTTPS Transport**: Pure-Rust `ureq` + `rustls` enforces `https://` for all remote endpoints (`http://127.0.0.1` is permitted strictly for local loopback IDE RPC) and caps every HTTP response body at `2 MiB` so a misbehaving endpoint can never exhaust memory.
- **Kernel-Backed Process-Group Reaping**: Every child CLI subprocess spawns in an isolated Unix process group (`process_group(0)`) with Linux `PR_SET_PDEATHSIG(SIGKILL)` and `kill(-pgid, SIGKILL)` timeout/teardown cleanup, guaranteeing zero orphaned processes when locking the screen or disabling the extension.
- **Read-Only Credential & Database Access**: Cursor's `state.vscdb` is opened with `mode=ro&immutable=1` via embedded `rusqlite`; Gemini CLI and Codex credentials are never rotated behind their CLIs' backs; and GNOME Shell's credential watcher uses `Gio.FileMonitor` filesystem event notifications exclusively without ever reading credential file contents in GJS.
- **Zero Resident Background Daemon**: Uses `0 MB` of idle background daemon RAM — the extension hydrates directly from `state.json` in `<1ms` on login/unlock and reacts to updates in `<2ms` via `Gio.FileMonitor`.

---

## Packaging & Developer Verification

```bash
# Run static process-boundary, motion-comfort, ClickGesture, syntax, schema, and secret checks
./scripts/check-guardrails.sh

# Run pure-JS unit tests (Node 22+ built-in test runner)
node --test tests/*.test.mjs

# Run Rust linter, test suite, and RustSec advisory audit
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo audit

# Build and verify an extensions.gnome.org (EGO) .shell-extension.zip bundle
./scripts/pack-extension.sh
# Output: ./target/extension-dist/agent-status@gnome-agent-status.shell-extension.zip
```

## License

This project uses the Apache License 2.0. Refer to the [LICENSE](LICENSE) file.
