#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXT_UUID="agent-status@gnome-agent-status"
BIN_DEST="${HOME}/.local/bin/gnome-agent-status"
EXT_DEST="${HOME}/.local/share/gnome-shell/extensions/${EXT_UUID}"

echo "==> Building gnome-agent-status (release)..."
cargo build --release --manifest-path "${SCRIPT_DIR}/Cargo.toml"

echo "==> Installing binary to ${BIN_DEST}..."
mkdir -p "$(dirname "${BIN_DEST}")"
install -m 0755 "${SCRIPT_DIR}/target/release/gnome-agent-status" "${BIN_DEST}"

echo "==> Initializing config (if not already present)..."
if "${BIN_DEST}" --init-config 2>/dev/null; then
    echo "    Config ready at ${HOME}/.config/gnome-agent-status/config.toml"
else
    echo "    Note: ${HOME}/.config is not writable in this environment; using the built-in default config (or run ./install.sh on host to write ~/.config/gnome-agent-status/config.toml)."
fi

echo "==> Installing GNOME Shell extension to ${EXT_DEST}..."
mkdir -p "${EXT_DEST}"
cp -f "${SCRIPT_DIR}/extension/metadata.json" "${EXT_DEST}/metadata.json"
cp -f "${SCRIPT_DIR}/extension/extension.js" "${EXT_DEST}/extension.js"
cp -f "${SCRIPT_DIR}/extension/stylesheet.css" "${EXT_DEST}/stylesheet.css"

if command -v gnome-extensions >/dev/null 2>&1; then
    echo "==> Enabling extension ${EXT_UUID}..."
    gnome-extensions enable "${EXT_UUID}" || true
fi

echo "==> Done! Note: On Wayland, log out and back in if this is the first time installing the extension."
