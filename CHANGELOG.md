# Changelog

All notable changes to `gnome-agent-status` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-11-11

### Added

- **10 Built-in AI Coding Agent Providers**:
  - **Claude Code (`claude`)** — OAuth usage endpoint, multi-bucket `Weekly` / `5-Hour` / model tiers / `Extra Usage`, passive `ingest-hook claude` (`statusLine`, `0` API calls), and `claude -p /usage` CLI fallback.
  - **OpenAI Codex (`codex`)** — ChatGPT usage API, `codex app-server` JSON-RPC (`account/rateLimits/read`), and offline session rollout `.jsonl` / `.jsonl.zst` zstd fallback.
  - **xAI Grok (`grok`)** — Rolling weekly compute pool and prepaid/on-demand credit balances with multi-account profile discovery.
  - **Cursor (`cursor`)** — Monthly cycle quotas (`Cursor Models`, `Other Models`, `Monthly`) via bundled `rusqlite` (`immutable=1` read-only access) and in-memory Electron `safeStorage` (`v10` / Linux `v11`) decryption.
  - **Google Antigravity (`agy`)** — 4 model-family buckets (`Gemini · Weekly`, `Gemini · 5-Hour`, `Claude & GPT · Weekly`, `Claude & GPT · 5-Hour`) combining `<2ms` local Linux `language_server` Connect-RPC with `agy -p /usage --output-format json`.
  - **OpenRouter (`openrouter`)** — Key usage, daily/weekly/monthly spend limits, remaining credit balances, and pay-as-you-go tracking via `GET /api/v1/key` and `/api/v1/credits`.
  - **GitHub Copilot (`copilot`)** — Monthly `Premium Requests` (`31d`), metered `Chat` / `Completions` buckets, and plan badges (`Free`, `Pro`, `Pro+`, `Business`, `Enterprise`).
  - **Gemini CLI (`gemini`)** — Read-only Cloud Code `v1internal:retrieveUserQuota` daily model-group quotas (`Gemini Pro · Daily`, `Gemini Flash · Daily`) with passive expiry fallback.
  - **Windsurf (`windsurf`)** — Local Linux `language_server_linux` Connect-RPC (`GetUserStatus`) prompt/flow credit buckets plus non-locking read-only SQLite (`state.vscdb`) fallback.
  - **DeepSeek (`deepseek`)** — Funded `USD` / `CNY` balance and availability monitoring via `GET /user/balance`.
- **Pixel-Aligned Official Iconography**:
  - Cute `16x16` symbolic robot top-bar indicator (`agent-status-symbolic.svg`) with calm severity states (`normal`, `80%` amber warning, `90%` coral-red critical) plus 10 crisp `24x24` vector provider marks (`extension/icons/provider-*.svg`).
- **24 Curated Color Themes & Dedicated Theme Picker (`ThemesPage`)**:
  - **Glass & Adaptive**: `GNOME System` (`system`), `Liquid Glass` (`liquid-glass` frosted dark smoky glass), and `Liquid Glass Light` (`liquid-glass-light` pearl daylight glass).
  - **Light Palettes**: `Catppuccin Latte`, `Flexoki Light`, `Rosé Pine Dawn`, `Pure Daylight`, and `Lumon Ice`, with high-contrast dark slate avatar medallions.
  - **Dark Palettes**: `Midnight Slate` (default), `Tokyo Night`, `Catppuccin Mocha`, `Everforest`, `Gruvbox`, `Kanagawa`, `Matte Black`, `Nord`, `Osaka Jade`, `Ristretto`, `Rosé Pine`, `Ethereal`, `Hackerman`, `Miasma`, `Vantablack`, and `Retro 82`.
  - Dedicated searchable/filterable `Themes` page (`All` | `Dark` | `Light` | `Glass`) with live in-place preview and WCAG contrast verification (`>= 10.0:1` primary, `>= 5.8:1` secondary).
- **2-Column Wide Bento & Progressive-Disclosure Popover**:
  - Configurable `popover-layout` (`Compact List` `352px` default with in-place `"Show N more agents"` / `"Show fewer"` disclosure after the top 5 cards, `Auto 2-Column` for 6+ visible agents, and `2-Column Wide` `688px` side-by-side Bento layout).
  - Multi-page drill-down architecture (`MainUsagePage`, `ProviderDetailPage`, `QuickOptionsPage`, `ThemesPage`) with multi-account switcher, 1-click `[Sign in]` terminal launcher, `Copy Usage Summary` clipboard button (`edit-copy-symbolic`), global keyboard shortcut (`keybinding-toggle-popover`), arrow-key card navigation, and opacity-fade hover/focus tooltips.
  - Right-click top-bar context menu (`Refresh Usage`, `Copy Usage Summary`, `Settings…`, `Quit Agent Status`).
- **7-Day Burn Sparkline & Sustainable Pace Indicators**:
  - Hourly quantized 7-day (`168`-point) burn-rate ring buffer (`sparkline`) rendered via Cairo `SparklineChart` on provider hero cards alongside live window pace projections (`Pace: Comfortable`, `Pace: On track`, `Pace: Elevated`, `Pace: N× sustainable`).
- **Smart Quota & Reset Desktop Notifications**:
  - Per-window deduplicated GNOME notifications at `80%` (*Warning*), `90%` (*Critical*), and window reset (*Quota Reset*), silently seeded on initial session hydration to avoid login/unlock alert spam.
- **Reactive Watchers, Status Bar Modes & Standalone systemd Timer**:
  - Automatic refresh on suspend/resume (`login1` `PrepareForSleep`), network reconnect (`Gio.NetworkMonitor`), and credential sign-in (`Gio.FileMonitor`).
  - `--waybar` JSON and `--tmux` status-bar output modes on both live collection and `<2ms` zero-network `cached` reads.
  - Hardened user units (`contrib/systemd/gnome-agent-status.{service,timer}`) for Waybar, Hyprland, Sway, and `tmux` environments outside GNOME Shell.
- **Diagnostics, Shell Completions, Manpage, Localization & Supply-Chain Tooling**:
  - `gnome-agent-status doctor [--json]` zero-secret configuration and permission health check.
  - `gnome-agent-status completions <bash|zsh|fish>` and `gnome-agent-status manpage` generators integrated into `install.sh` and `uninstall.sh`.
  - GNU gettext localization template (`po/agent-status.pot`), EGO bundle packager (`scripts/pack-extension.sh`), static guardrail suite (`scripts/check-guardrails.sh`), `cargo-deny` v2 policy (`deny.toml`), `SECURITY.md`, and GitHub Actions CI + attested multi-arch release workflows.
- **Libadwaita Preferences Overhaul (`extension/prefs.js`)**:
  - Three searchable GTK4/Libadwaita pages covering panel modes (`Minimal Icon`, `Compact Pills`, `Full Text`), 24 themes, popover layout, global toggle shortcut capture dialog, notification switches, 10-provider ordering & visibility (`Reset to Default Top 5`), and manual account location management (`config upsert-account` / `remove-account`).

### Changed

- Upgraded output schema to `StatusReport` v2 (`providers[] -> accounts[] -> buckets[]`) while retaining backwards-compatible parsing for v0.1 `[[agents]]` configuration entries and v1 report payloads.
- Replaced external `sqlite3` CLI and OpenSSL dependencies with bundled `rusqlite` and pure-Rust `ureq` + `rustls`, enabling `<1ms` synchronous `state.json` hydration in GNOME Shell 45–50.
- Enforced GNOME Shell 50 `Clutter.ClickGesture` compatibility and zero-scale opacity crossfades honoring `org.gnome.desktop.interface` `enable-animations`.

### Security

- Hardened all cache, state, lock, and config writes (`0600` files inside `0700` non-symlink directories) using `O_NOFOLLOW`, `create_new(true)` unique temp files, `flock` serialization, and atomic `rename()`.
- Enforced `https://` for all non-loopback HTTP requests, capped HTTP response bodies at `2 MiB`, and added automatic credential redaction (`redact_secrets`) across all error and diagnostic paths.
- Isolated child CLI subprocesses in dedicated Unix process groups (`process_group(0)`) with Linux `PR_SET_PDEATHSIG(SIGKILL)` kernel reaping and `kill(-pgid, SIGKILL)` timeout cleanup.

## [0.1.0] - 2026-11-06

### Added

- Initial `gnome-agent-status` Rust CLI collector with `config.toml` (`[[agents]]`) configuration, JSON output, and aligned `--text` terminal table output with local ISO 8601 reset timestamps.
- Initial GNOME Shell top-bar indicator extension (`agent-status@gnome-agent-status`) displaying periodic usage summaries with manual refresh and config shortcuts under the Apache-2.0 license.

[0.2.0]: https://github.com/ToxicBakery/gnome-agent-status/releases/tag/v0.2.0
[0.1.0]: https://github.com/ToxicBakery/gnome-agent-status/commits/a7dccf2
