# Contributing to `gnome-agent-status`

Thank you for contributing to `gnome-agent-status`! This guide covers the repository layout, GNOME Shell 45–50 & GTK4 architectural invariants, how to add new AI coding providers or WCAG-verified themes, and how to run the local verification and packaging suite.

---

## Repository Layout

| Path | Responsibility |
| :--- | :--- |
| `src/` | Rust collector CLI (`gnome-agent-status`): config & `toml_edit` writers ([`src/config.rs`](src/config.rs)), `O_NOFOLLOW` + `0600` atomic cache & 7-day hourly sparklines ([`src/cache.rs`](src/cache.rs)), `https://`-only remote / loopback HTTP guard ([`src/http.rs`](src/http.rs)), isolated process-group runner with Linux `PR_SET_PDEATHSIG` ([`src/proc_exec.rs`](src/proc_exec.rs)), `--waybar` / `--tmux` / `--text` formatters ([`src/text.rs`](src/text.rs)), zero-secret `doctor` diagnostics ([`src/doctor.rs`](src/doctor.rs)), and CLI subcommands ([`src/cli.rs`](src/cli.rs)). |
| `src/providers/` | Pluggable `UsageProvider` trait, account deduplication, and parallel collection engine ([`src/providers/mod.rs`](src/providers/mod.rs)) plus the 10 built-in provider backends (`claude`, `codex`, `grok`, `cursor`, `antigravity`, `openrouter`, `copilot`, `gemini`, `windsurf`, `deepseek`). |
| `extension/` | GNOME Shell 45–50 extension lifecycle ([`extension/extension.js`](extension/extension.js)), top-bar indicator & right-click menu ([`extension/indicator.js`](extension/indicator.js)), async subprocess + `Gio.FileMonitor` / `login1` / `NetworkMonitor` coordinator ([`extension/backend.js`](extension/backend.js)), deduplicated quota notifications ([`extension/notifications.js`](extension/notifications.js)), GNOME 45–50 & gettext compatibility shims ([`extension/compat.js`](extension/compat.js)), pure-JS shared model/theme helpers ([`extension/format.js`](extension/format.js)), standalone GTK4/Libadwaita Preferences ([`extension/prefs.js`](extension/prefs.js)), St stylesheet ([`extension/stylesheet.css`](extension/stylesheet.css)), vector brand icons (e.g. [`extension/icons/agent-status-symbolic.svg`](extension/icons/agent-status-symbolic.svg)), and GSettings schema ([`extension/schemas/org.gnome.shell.extensions.agent-status.gschema.xml`](extension/schemas/org.gnome.shell.extensions.agent-status.gschema.xml)). |
| `extension/popover/` | Multi-page GNOME Shell popover UI: view router ([`extension/popover/controller.js`](extension/popover/controller.js)), top-5 / Bento overview ([`extension/popover/mainPage.js`](extension/popover/mainPage.js)), provider drill-down & account switcher ([`extension/popover/detailPage.js`](extension/popover/detailPage.js)), quick options ([`extension/popover/optionsPage.js`](extension/popover/optionsPage.js)), 24-preset Theme Studio ([`extension/popover/themesPage.js`](extension/popover/themesPage.js)), and reusable St/Cairo widgets including `SparklineChart` ([`extension/popover/widgets.js`](extension/popover/widgets.js)). |
| `scripts/` | Static architectural & security guardrail suite ([`scripts/check-guardrails.sh`](scripts/check-guardrails.sh)) and `extensions.gnome.org` (EGO) `.shell-extension.zip` bundler ([`scripts/pack-extension.sh`](scripts/pack-extension.sh)). |
| `tests/` | Pure-JS unit, shell-quoting security, and 24-theme + `system-light` WCAG 2.1 contrast verification suite ([`tests/format.test.mjs`](tests/format.test.mjs)), paired with hermetic Rust integration tests ([`src/integration_tests.rs`](src/integration_tests.rs)). |
| `contrib/systemd/` | Optional hardened `systemd --user` service ([`contrib/systemd/gnome-agent-status.service`](contrib/systemd/gnome-agent-status.service)) and timer ([`contrib/systemd/gnome-agent-status.timer`](contrib/systemd/gnome-agent-status.timer)) units for Waybar, Sway, Hyprland, and `tmux` setups outside GNOME Shell. |

---

## Core Architectural Rules

All pull requests are gated by [`scripts/check-guardrails.sh`](scripts/check-guardrails.sh), which enforces these hard invariants:

### 1. GNOME Shell vs GTK4/Libadwaita Process Boundary

GNOME Shell extensions execute across **two strictly separated OS processes**:

- **GNOME Shell Compositor (`extension/*.js` except `prefs.js`, plus `extension/popover/*.js`)**:
  - Runs inside `gnome-shell` (`Clutter`, `St`, `Meta`, `Shell`).
  - **NEVER** import `gi://Gtk`, `gi://Adw`, or `gi://Gdk` — loading GTK4 inside `gnome-shell` crashes the user's compositor session.
  - **NEVER** read credential files, query SQLite databases, or make network calls from GJS; hydrate strictly from `$XDG_CACHE_HOME/gnome-agent-status/state.json`.
- **Preferences Window ([`extension/prefs.js`](extension/prefs.js))**:
  - Runs inside the standalone `gnome-extensions-app` GTK4/Libadwaita process.
  - **NEVER** import `gi://St`, `gi://Clutter`, `gi://Shell`, `gi://Meta`, or `resource:///org/gnome/shell/ui/*`.
  - Escape all dynamic Pango markup strings (`title`, `subtitle`, toast text) with `GLib.markup_escape_text`.
- **Shared Pure-JS Layer ([`extension/format.js`](extension/format.js))**:
  - **MUST** contain zero `gi://` or `resource:///` imports so it can be imported identically by `gnome-shell`, `prefs.js`, and `node --test`.

### 2. Motion Comfort (Zero Scale Transforms)

- Never use `scale`, `scale_x`, `scale_y`, `set_scale`, `'scale-x'`, `'scale-y'`, or CSS `scale(...)` transforms anywhere in `extension/*.js`, `extension/popover/*.js`, or [`extension/stylesheet.css`](extension/stylesheet.css).
- Route actor transitions through `easeActorOpacity()` in [`extension/compat.js`](extension/compat.js), which checks `org.gnome.desktop.interface` `enable-animations` and snaps immediately (`0ms`) when desktop animations are disabled.

### 3. GNOME Shell 45–50 & `Clutter.ClickGesture` Compatibility

- GNOME Shell 50 migrated `St.Button` and `PanelMenu.Button` click recognition to `Clutter.ClickGesture`, which claims pointer event sequences before legacy `button-press-event` / `button-release-event` listeners run on child widgets.
- **NEVER** connect `button-press-event` or `button-release-event` in `extension/popover/*.js` or `extension/*.js`.
  - Use `St.Button` `'clicked'` signals for primary activation inside popovers.
  - For secondary-click menus on the top-bar `PanelMenu.Button` ([`extension/indicator.js`](extension/indicator.js)), attach `Clutter.ClickGesture` (`set_required_button(Clutter.BUTTON_SECONDARY)`) with a guarded legacy fallback for GNOME 45–48.
- Use the version-safe layout helpers in [`extension/compat.js`](extension/compat.js) (`createVBox`, `createHBox`, `setScrollChild`) so code runs cleanly across GNOME 45 through 50 without deprecation warnings.

### 4. GNOME `St` CSS Restrictions

GNOME Shell's `St` stylesheet engine is not a full web browser engine. In [`extension/stylesheet.css`](extension/stylesheet.css), **do not use**:

- CSS custom properties (`var(--...)`)
- `calc(...)` expressions
- Flexbox/grid `gap:` properties
- Percentage widths (`width: 50%`, `min-width: 100%`)
- `scale(...)` transforms

Use explicit pixel padding/margins and `x_expand: true` Clutter layout properties instead.

### 5. Security & Read-Only Credential Hygiene

Review [`SECURITY.md`](SECURITY.md) before touching credential discovery, HTTP, or file I/O:

- Open third-party credential files strictly **read-only** and never rotate OAuth refresh tokens behind upstream CLIs' backs.
- Open IDE SQLite databases (`state.vscdb`) via bundled `rusqlite` with `mode=ro&immutable=1` and `PRAGMA query_only = ON`.
- Route all outbound requests through [`src/http.rs`](src/http.rs) (`https://` required for all non-loopback hosts; `2 MiB` bounded reads) and scrub error messages with `redact_secrets`.
- Write local cache/config files exclusively through `atomic_write_0600` (`O_NOFOLLOW`, `create_new(true)`, mode `0600` inside `0700` non-symlink directories).

---

## Adding a New AI Provider

1. **Canonical ID & Model**: Add the lowercase provider slug to `CANONICAL_PROVIDER_IDS` in [`src/model.rs`](src/model.rs).
2. **Rust Backend**: Implement `UsageProvider` in `src/providers/<id>.rs` using `FetchContext` (`ctx.http`, `ctx.home_dir`, `ctx.xdg_config_dir`, `ctx.env_var(...)`) so unit tests remain 100% hermetic without touching real `$HOME` or live external APIs, and register it in `ProviderRegistry::builtin()` in [`src/providers/mod.rs`](src/providers/mod.rs).
3. **Config, CLI & Diagnostics**: Wire default provider config in [`src/config.rs`](src/config.rs) and [`config.example.toml`](config.example.toml), zero-PII discovery reporting in [`src/doctor.rs`](src/doctor.rs), and help docstrings in [`src/cli.rs`](src/cli.rs).
4. **Vector Brand Icon**: Add a crisp `24x24` `viewBox` SVG icon (zero embedded raster bitmaps) at `extension/icons/provider-<id>.svg`.
5. **UI Metadata & Schema**: Add the provider entry to `DEFAULT_PROVIDER_ORDER`, `DEFAULT_HIDDEN_PROVIDERS` (if outside the default Top 5), `CANONICAL_PROVIDERS`, and `defaultCredentialWatchPaths()` in [`extension/format.js`](extension/format.js), and update default arrays in [`extension/schemas/org.gnome.shell.extensions.agent-status.gschema.xml`](extension/schemas/org.gnome.shell.extensions.agent-status.gschema.xml).
6. **Tests**: Add mock-HTTP / temp-directory Rust tests in `src/providers/<id>.rs` & [`src/integration_tests.rs`](src/integration_tests.rs), and metadata/normalization assertions in [`tests/format.test.mjs`](tests/format.test.mjs).

---

## Adding a New Theme Preset & WCAG Contrast Gates

1. **Register Palette Metadata**: Add the preset object to `THEME_PRESETS` in [`extension/format.js`](extension/format.js) with `id`, `label`, `subtitle`, `category` (`'dark' | 'light' | 'glass'`), `isLight`, `swatchHex`, `accentHex`, `bgHex`, `textHex`, `mutedHex`, `cardHex`, and Cairo progress-bar RGBA stops.
2. **Author St Stylesheet Rules**: Add `.gas-popover.theme-<id>` rules in [`extension/stylesheet.css`](extension/stylesheet.css) covering root surface, `.gas-provider-card`, `.gas-hero-card`, `.gas-segmented-track`, `.gas-segmented-btn.is-active`, `.gas-theme-row.is-active`, `.gas-account-row.is-selected`, `.gas-avatar-ring`, semantic `.is-warning` / `.is-critical`, and `.gas-error-banner`.
3. **Pass Automated WCAG 2.1 Contrast Gates**: [`tests/format.test.mjs`](tests/format.test.mjs) composites translucent surfaces over `bgHex` and enforces these minimum contrast ratios across every preset (plus `system-light`):
   - **Primary text** (`root`, `.gas-provider-name`, `.gas-hero-account-title`): **`>= 10.0:1`** (WCAG AAA+)
   - **Secondary & muted chrome** (subtitles, plan badges, reset countdowns, bucket labels, chevrons, section headers, sparkline titles, inactive segmented tabs): **`>= 5.8:1`**
   - **Active pills & selected rows** (`.gas-segmented-btn.is-active`, `.gas-theme-row.is-active`, `.gas-account-row.is-selected`): **`>= 5.5:1`**
   - **Light-surface avatar medallions** (`isLight: true`): `.gas-avatar-ring` must use `#222634` dark slate (**`>= 10.0:1`** contrast for white vector logos)
   - **Semantic status & error banners**: warning/critical percentages **`>= 4.8:1`**, `.gas-error-banner` **`>= 5.0:1`**

---

## Local Verification & Packaging Suite

Run the full check suite locally before opening a pull request:

```bash
# 1. Static guardrails (8/8: process boundaries, motion comfort, ClickGesture, JS/bash syntax, GSettings schema, secret & brand hygiene)
./scripts/check-guardrails.sh

# 2. Pure-JS unit, shell-quoting security, and 24-theme WCAG contrast tests (Node.js 22+)
node --test tests/format.test.mjs

# 3. Hermetic Rust test suite & zero-warning Clippy lints
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings

# 4. Supply-chain & license policy checks
cargo audit
cargo deny check

# 5. Build and validate an extensions.gnome.org (EGO) .shell-extension.zip bundle
./scripts/pack-extension.sh

# 6. Optional: install release binary, shell completions, manpage, and local GNOME Shell extension
./install.sh
```
