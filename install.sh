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
    glib-compile-schemas --strict "${EXT_DEST}/schemas"
else
    echo "    Warning: glib-compile-schemas not found; install glib2 development tools so extension preferences and GSettings can load."
fi

gvariant_str_array_append() {
    local current="$1"
    local item="$2"

    current="${current#"${current%%[![:space:]]*}"}"
    current="${current%"${current##*[![:space:]]}"}"

    if [[ ! "${current}" =~ ^(@as[[:space:]]*)?\[.*\]$ ]]; then
        return 1
    fi

    if [[ "${current}" == *"'${item}'"* || "${current}" == *"\"${item}\""* ]]; then
        printf '%s\n' "${current}"
        return 0
    fi

    if [[ "${current}" =~ ^(@as[[:space:]]*)?\[[[:space:]]*\]$ ]]; then
        printf "['%s']\n" "${item}"
        return 0
    fi

    local prefix="${current%]}"
    prefix="${prefix%"${prefix##*[![:space:]]}"}"
    prefix="${prefix%,}"
    printf "%s, '%s']\n" "${prefix}" "${item}"
}

gvariant_str_array_remove() {
    local current="$1"
    local item="$2"

    current="${current#"${current%%[![:space:]]*}"}"
    current="${current%"${current##*[![:space:]]}"}"

    if [[ ! "${current}" =~ ^(@as[[:space:]]*)?\[.*\]$ ]]; then
        return 1
    fi

    local updated="${current}"
    while [[ "${updated}" == *"'${item}'"* ]]; do
        local inner="${updated#@as}"
        inner="${inner#"${inner%%[![:space:]]*}"}"
        inner="${inner#\[}"
        inner="${inner%\]}"
        inner="${inner#"${inner%%[![:space:]]*}"}"
        inner="${inner%"${inner##*[![:space:]]}"}"
        if [[ "${inner}" == "'${item}'" ]]; then
            printf '@as []\n'
            return 0
        fi

        if [[ "${updated}" == *"'${item}', "* ]]; then
            updated="${updated/"'${item}', "/}"
        elif [[ "${updated}" == *"'${item}',"* ]]; then
            updated="${updated/"'${item}',"/}"
        elif [[ "${updated}" == *", '${item}'"* ]]; then
            updated="${updated/", '${item}'"/}"
        elif [[ "${updated}" == *",'${item}'"* ]]; then
            updated="${updated/",'${item}'"/}"
        else
            break
        fi
    done
    printf '%s\n' "${updated}"
}

pre_enable_extension_gsettings() {
    local uuid="$1"
    if ! command -v gsettings >/dev/null 2>&1; then
        return 1
    fi

    local current_enabled updated_enabled
    if ! current_enabled="$(gsettings get org.gnome.shell enabled-extensions 2>/dev/null)"; then
        return 1
    fi
    if ! updated_enabled="$(gvariant_str_array_append "${current_enabled}" "${uuid}")"; then
        return 1
    fi

    if [[ "${updated_enabled}" != "${current_enabled}" ]]; then
        if ! gsettings set org.gnome.shell enabled-extensions "${updated_enabled}" 2>/dev/null; then
            return 1
        fi
    fi

    local current_disabled updated_disabled
    if current_disabled="$(gsettings get org.gnome.shell disabled-extensions 2>/dev/null)"; then
        if [[ "${current_disabled}" == *"'${uuid}'"* ]]; then
            if updated_disabled="$(gvariant_str_array_remove "${current_disabled}" "${uuid}")"; then
                gsettings set org.gnome.shell disabled-extensions "${updated_disabled}" 2>/dev/null || true
            fi
        fi
    fi

    local verify_enabled
    verify_enabled="$(gsettings get org.gnome.shell enabled-extensions 2>/dev/null || true)"
    [[ "${verify_enabled}" == *"'${uuid}'"* ]]
}

if command -v gnome-extensions >/dev/null 2>&1 && gnome-extensions info "${EXT_UUID}" >/dev/null 2>&1; then
    echo "==> Enabling extension ${EXT_UUID}..."
    if ! gnome-extensions enable "${EXT_UUID}" 2>/dev/null; then
        pre_enable_extension_gsettings "${EXT_UUID}" || true
    fi
    echo "==> Done! If updating an already-loaded extension on Wayland, log out and back in to reload ES modules."
else
    if pre_enable_extension_gsettings "${EXT_UUID}"; then
        echo "==> Pre-enabled ${EXT_UUID} in GNOME Shell settings for next login."
        echo "==> Done! GNOME Shell (Wayland) discovers newly installed extensions at session login."
        echo "    Log out and back in — ${EXT_UUID} will activate automatically."
    else
        echo "==> Done! GNOME Shell (Wayland) discovers newly installed extensions at session login."
        echo "    Log out and back in, then enable with: gnome-extensions enable ${EXT_UUID}"
    fi
fi
