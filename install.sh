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
    echo "    Note: ${HOME}/.config is not writable in this environment; using built-in defaults (or run ./install.sh on host to write ~/.config/gnome-agent-status/config.toml)."
fi

echo "==> Installing GNOME Shell extension to ${EXT_DEST}..."
rm -rf "${EXT_DEST}/popover" "${EXT_DEST}/icons" "${EXT_DEST}/schemas"
mkdir -p "${EXT_DEST}/popover" "${EXT_DEST}/icons" "${EXT_DEST}/schemas"

cp -f "${SCRIPT_DIR}/extension/metadata.json" "${EXT_DEST}/metadata.json"
cp -f "${SCRIPT_DIR}/extension/stylesheet.css" "${EXT_DEST}/stylesheet.css"
cp -f "${SCRIPT_DIR}"/extension/*.js "${EXT_DEST}/"
cp -f "${SCRIPT_DIR}"/extension/popover/*.js "${EXT_DEST}/popover/"
cp -f "${SCRIPT_DIR}"/extension/icons/*.svg "${EXT_DEST}/icons/"
cp -f "${SCRIPT_DIR}"/extension/schemas/*.gschema.xml "${EXT_DEST}/schemas/"

if command -v glib-compile-schemas >/dev/null 2>&1; then
    echo "==> Compiling GSettings schemas..."
    glib-compile-schemas "${EXT_DEST}/schemas"
else
    echo "    Warning: glib-compile-schemas not found; install glib2 development tools so extension preferences and GSettings can load."
fi

if command -v gnome-extensions >/dev/null 2>&1; then
    echo "==> Enabling extension ${EXT_UUID}..."
    gnome-extensions enable "${EXT_UUID}" || true
fi

echo "==> Done! Note: On Wayland, log out and back in if this is the first time installing the extension."
