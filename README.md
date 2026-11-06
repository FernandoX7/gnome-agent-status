# gnome-agent-status

Displays 7-day usage limits (`% used`) for configured AI coding agents (`agy` and `claude`) in the GNOME Shell top bar, refreshed automatically every 5 minutes.

## Architecture

1. **Rust Collector (`gnome-agent-status`)**:
   - Reads `~/.config/gnome-agent-status/config.toml` (or `.json`).
   - Queries all configured agents in parallel using `std::thread::scope` with per-agent timeouts.
   - **Antigravity (`agy`)**: Runs `agy -p /usage --output-format json`, extracts the `Gemini Models` weekly bucket (`gemini-weekly`), and converts `remaining_fraction` to percentage used (`100 - remaining%`).
   - **Claude (`claude`)**: Runs `claude -p /usage` with `CLAUDE_CONFIG_DIR=<expanded_path>`, extracts `Current week (all models): <N>% used`, and ignores model-specific buckets like `(Fable)`.
   - Outputs a single JSON payload to `stdout`.

2. **Thin GNOME Shell 45+ Extension (`extension/`)**:
   - Runs `gnome-agent-status` asynchronously via `Gio.Subprocess` + `communicate_utf8_async` on startup and every 300 seconds (`GLib.timeout_add_seconds`).
   - Updates the top-bar label (e.g. `AGY: 16% | Claude: 89%`) and populates a dropdown menu with per-agent details, reset times, warnings, a **Refresh Now** button, and an **Open Config** shortcut.

## Configuration

Location: `~/.config/gnome-agent-status/config.toml`

```toml
separator = " | "
timeout_seconds = 30

[[agents]]
name = "AGY"
type = "agy"

[[agents]]
name = "Claude"
type = "claude"
path = "~/.claude-work"
# Optional explicit binary override (auto-discovers ~/.local/bin/claude by default):
# binary = "/usr/local/bin/claude"
```

## Installation (on Host)

```bash
./install.sh
```

This builds the release binary to `~/.local/bin/gnome-agent-status`, initializes `~/.config/gnome-agent-status/config.toml` if missing, and installs the GNOME Shell extension to `~/.local/share/gnome-shell/extensions/agent-status@gnome-agent-status/`.

## Terminal Text Mode

Use the `--text` flag to print one status line per configured agent in a terminal.

```bash
gnome-agent-status --text
```

Each line shows the agent name, the percentage used (or `--`), the reset time, and any error text.

Add the `--color` flag to show ANSI colors in the percentage column.
Values below 50% are green, values from 50% to 79% are yellow, and values at 80% or above are red.
Errors also show in red.

```bash
gnome-agent-status --text --color
```

### Periodic Updates with `watch`

Run the binary under `watch` to check agent status in a terminal:

```bash
watch -n 120 gnome-agent-status --text
```

Each run calls the agent CLIs.
Those CLI calls can be slow, so an interval of 60 to 300 seconds is sensible.
The GNOME extension uses a 300-second interval.

To keep ANSI colors under `watch`, pass `--color` to both commands:

```bash
watch --color -n 120 gnome-agent-status --text --color
```

To check a remote host over SSH, run `watch` in an interactive SSH session:

```bash
ssh -t host 'watch -n 120 ~/.local/bin/gnome-agent-status --text'
```

## License

This project uses the Apache License 2.0. Refer to the [LICENSE](LICENSE) file.
