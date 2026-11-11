# Security Policy & Architecture

## Supported Versions

Security fixes are maintained for the latest minor release series:

| Version | Supported |
| :--- | :--- |
| `0.2.x` | Yes |
| `< 0.2.0` | No — upgrade to `0.2.0` or newer |

## Reporting a Vulnerability

Please **do not** report security vulnerabilities through public GitHub issues, discussions, or pull requests.

Instead, report suspected vulnerabilities privately via **[GitHub Security Advisories](https://github.com/ToxicBakery/gnome-agent-status/security/advisories/new)**.

When submitting a report, please include:

1. Affected version or commit SHA and operating system / GNOME Shell version.
2. Component involved (`gnome-agent-status` Rust collector CLI, GNOME Shell extension runtime, Libadwaita Preferences, or packaging scripts).
3. Step-by-step reproduction instructions or a minimal proof-of-concept.
4. Assessment of potential impact (e.g., local credential exposure, symlink/TOCTOU file clobber, remote transport downgrade, or compositor stability).

Maintainers aim to acknowledge vulnerability reports within **72 hours**, confirm triage status within **7 days**, and coordinate an advisory and patched release once a fix is verified.

---

## Threat Model & Trust Boundaries

`gnome-agent-status` inspects local quota metadata across developer AI tools whose directories also hold sensitive OAuth credentials, API keys, and IDE SQLite databases. Its architecture enforces strict privilege and process separation across three execution contexts:

1. **GNOME Shell Compositor Process (`extension/*.js`, `extension/popover/*.js`)**
   - Never reads credential files, API keys, or SQLite databases.
   - Never performs network requests or imports `Gtk` / `Adw` / `Gdk`.
   - Hydrates strictly from `$XDG_CACHE_HOME/gnome-agent-status/state.json` (`0600`) and uses `Gio.FileMonitor` solely for filesystem change notifications.
2. **Libadwaita Preferences Process (`extension/prefs.js`)**
   - Runs isolated inside `gnome-extensions-app` with zero compositor imports (`St` / `Clutter` / `Shell`).
   - Escapes all dynamic labels, subtitles, and toasts via `GLib.markup_escape_text`.
   - Validates local paths (`file.get_path()`) from `Gtk.FileDialog` and serializes `config.toml` updates through `gnome-agent-status config` CLI subcommands.
3. **Short-Lived Rust Collector (`gnome-agent-status`)**
   - Runs with **zero resident background daemon** (`0 MB` idle RAM), executing bounded collection passes and exiting cleanly.

---

## Defense-in-Depth Security Controls

### 1. Strictly Read-Only Credential Access & Zero Token Rotation

- **Read-only inspection**: Local credential stores (`~/.claude*`, `~/.codex*`, `~/.grok*`, `~/.gemini*`, `~/.config/github-copilot/*`, `~/.config/openrouter/*`, `~/.config/deepseek/*`) are opened strictly read-only.
- **Zero background token rotation**: The collector never refreshes or rotates OAuth refresh tokens behind upstream CLIs' backs. If an OAuth access token expires, `gnome-agent-status` degrades gracefully to passive cached state rather than racing single-use refresh tokens used by Claude Code, Codex, Gemini CLI, or Grok.
- **Non-locking SQLite queries**: Cursor and Windsurf `state.vscdb` databases are queried via embedded `rusqlite` with `mode=ro&immutable=1` and `PRAGMA query_only = ON`, preventing write locks or journal mutations on running IDEs.
- **In-memory keyring decryption**: Electron `safeStorage` (`v10` / Linux `v11`) token decryption executes solely in process memory and is never written to disk.

### 2. Zero Secrets on Disk or Diagnostic Output

- `$XDG_CACHE_HOME/gnome-agent-status/state.json` and `collector-cache.json` persist only normalized quota percentages, reset epoch timestamps, plan tier badges, hourly burn-rate sparkline points (`{ts, used}`), and sanitized status notes.
- Raw OAuth tokens, refresh tokens, session cookies, local Connect-RPC `--csrf_token` values, and vendor API keys (`sk-or-*`, `sk-ant-*`, `sk-*`) are **never** written to cache files, stdout (`--waybar`, `--tmux`, `--text`, JSON), or `gnome-agent-status doctor` diagnostics.

### 3. Symlink-Safe `O_NOFOLLOW` + `0600` Atomic Writes Inside `0700` Directories

- **Directory hardening (`ensure_dir_0700`)**: `$XDG_CONFIG_HOME/gnome-agent-status` and `$XDG_CACHE_HOME/gnome-agent-status` are checked with `lstat` (`fs::symlink_metadata`) to reject symlinked directories and tightened to mode `0700` on user-owned directories.
- **TOCTOU-resistant atomic writes (`atomic_write_0600`)**: Every write to `state.json`, `collector-cache.json`, `config.toml`, `.collector.lock`, or temporary SQLite WAL snapshots:
  1. Verifies the target path is not a symbolic link (`lstat`).
  2. Acquires an exclusive advisory `flock(LOCK_EX)` lock opened with `0600` and `O_NOFOLLOW`.
  3. Creates a unique `.tmp.<pid>.<nonce>.<nanos>` sibling file using `create_new(true)`, mode `0600`, and `O_NOFOLLOW`.
  4. Flushes and `fsync`s (`sync_all()`) file contents, re-verifies the destination via `lstat`, and atomically commits via POSIX `rename()`.

### 4. `https://`-Only Non-Loopback Network Guard & `2 MiB` Bounded Reads

- **Pure-Rust TLS**: Uses `ureq` with `rustls` (`openssl` and `openssl-sys` are explicitly denied in [`deny.toml`](deny.toml)).
- **HTTPS transport guard (`validate_remote_or_loopback_url`)**: Every outbound HTTP request is validated before connection setup. All remote endpoints must use `https://`; cleartext `http://` is permitted **exclusively** for local loopback addresses (`127.0.0.1`, `localhost`, `::1`, `[::1]`) used by local IDE Connect-RPC servers and hermetic tests. `post_loopback_json` additionally rejects any non-loopback target even over `https://`.
- **Bounded response bodies (`MAX_HTTP_BODY_BYTES`)**: Every HTTP response body reader is capped at **`2 MiB` (`2,097,152` bytes)** via `Read::take`, ensuring a misbehaving or malicious upstream endpoint cannot trigger unbounded memory allocation.
- **URL query scrubbing**: Transport error context strips query parameters (`redact_url`) prior to error formatting.

### 5. Automatic Credential Redaction in Errors & Logs

All upstream HTTP errors and subprocess diagnostic strings pass through `redact_secrets` before reaching `state.json`, `collector-cache.json`, or `stderr`. The sanitizer scrubs:

- `Authorization` headers (`Bearer`, `token`, `Basic`) and standalone `Bearer <secret>` / `token <secret>` sequences
- JSON Web Tokens (`eyJ*`)
- Vendor API and OAuth token prefixes: Anthropic (`sk-ant-*`), OpenRouter (`sk-or-v1-*`), OpenAI / DeepSeek (`sk-*`), xAI (`xai-*`), GitHub (`gho_*`, `ghu_*`, `ghp_*`, `ghs_*`, `ghr_*`, `github_pat_*`), and Google (`AIza*`, `ya29.*`, `1//*`)
- CLI flags and key/value fields (`--csrf_token`, `access_token`, `refresh_token`, `oauth_token`, `id_token`, `api_key`, `client_secret`, `session_token`, `workosCursorSessionToken`)

### 6. Process-Group Isolation & `PR_SET_PDEATHSIG` Child Reaping

- **Isolated Unix process groups**: Every fallback CLI subprocess (`claude`, `codex`, `agy`, `gh`, etc.) spawns with `process_group(0)` (`PGID == child_pid`) and `stdin` closed (`Stdio::null()`) unless piped explicitly.
- **Kernel-enforced parent-death reaping**: On Linux, a `pre_exec` hook invokes `prctl(PR_SET_PDEATHSIG, SIGKILL)` and immediately checks `getppid() == expected_ppid` (closing the post-`fork` parent-exit race). If `gnome-agent-status` is terminated abruptly (such as `Gio.Subprocess.force_exit()` when GNOME Shell locks the screen), the Linux kernel reaps any child CLI immediately.
- **Whole-tree timeout cleanup**: Timeouts and early JSON-RPC completions terminate the entire child process group via `kill(-pgid, SIGKILL)` (strictly guarded by `pid > 1`).

### 7. Supply-Chain & CI Guardrails

Every change is verified in continuous integration via:

- `./scripts/check-guardrails.sh` — static enforcement of compositor/GTK4 process boundaries, motion-comfort invariants, GNOME 50 `Clutter.ClickGesture` usage, GSettings schema validity, and tracked-file secret scanning.
- `cargo audit` & `cargo deny check` ([`deny.toml`](deny.toml)) — RustSec advisory scanning, license allowlisting, OpenSSL bans, and crates.io source restrictions.
- Build provenance attestations (`actions/attest-build-provenance`) and `SHA256SUMS` on tagged GitHub Releases.

---

## Auditing Local Permissions

You can verify local config (`0600`), cache directory (`0700`), state file (`0600`), and credential file permissions at any time without printing any secrets:

```bash
gnome-agent-status doctor
```
