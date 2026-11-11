#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXT_UUID="agent-status@gnome-agent-status"
ENABLE_TIMER=0

usage() {
    cat <<'EOF'
Usage: ./install.sh [OPTIONS]

Builds and installs the gnome-agent-status CLI, shell completions, manual page,
optional systemd user timer units, and GNOME Shell extension.

Options:
  --enable-timer  Enable and start gnome-agent-status.timer via systemctl --user
                  (useful for Waybar, Sway, Hyprland, or tmux outside GNOME Shell)
  -h, --help      Show this help message and exit
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --enable-timer)
            ENABLE_TIMER=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Error: unknown option '$1'" >&2
            usage >&2
            exit 2
            ;;
    esac
done

BIN_DIR="${HOME}/.local/bin"
BIN_DEST="${BIN_DIR}/gnome-agent-status"
DATA_HOME="${XDG_DATA_HOME:-${HOME}/.local/share}"
CONFIG_HOME="${XDG_CONFIG_HOME:-${HOME}/.config}"
EXT_DEST="${DATA_HOME}/gnome-shell/extensions/${EXT_UUID}"

echo "==> Building gnome-agent-status (release)..."
cargo build --release --manifest-path "${SCRIPT_DIR}/Cargo.toml"

echo "==> Installing binary to ${BIN_DEST}..."
mkdir -p "${BIN_DIR}"
install -m 0755 "${SCRIPT_DIR}/target/release/gnome-agent-status" "${BIN_DEST}"

echo "==> Initializing config (if not already present)..."
if "${BIN_DEST}" --init-config 2>/dev/null; then
    echo "    Config ready at ${CONFIG_HOME}/gnome-agent-status/config.toml"
else
    echo "    Note: ${CONFIG_HOME} is not writable in this environment; using built-in defaults (or run ./install.sh on host to write ${CONFIG_HOME}/gnome-agent-status/config.toml)."
fi

if "${BIN_DIR}/gnome-agent-status" completions bash >/dev/null 2>&1; then
    echo "==> Installing shell completions (bash, zsh, fish)..."
    BASH_COMP_DIR="${DATA_HOME}/bash-completion/completions"
    ZSH_COMP_DIR="${DATA_HOME}/zsh/site-functions"
    FISH_COMP_DIR="${CONFIG_HOME}/fish/completions"

    if mkdir -p "${BASH_COMP_DIR}" 2>/dev/null && [[ -w "${BASH_COMP_DIR}" ]]; then
        "${BIN_DIR}/gnome-agent-status" completions bash > "${BASH_COMP_DIR}/gnome-agent-status"
    fi
    if mkdir -p "${ZSH_COMP_DIR}" 2>/dev/null && [[ -w "${ZSH_COMP_DIR}" ]]; then
        "${BIN_DIR}/gnome-agent-status" completions zsh > "${ZSH_COMP_DIR}/_gnome-agent-status"
    fi
    if mkdir -p "${FISH_COMP_DIR}" 2>/dev/null && [[ -w "${FISH_COMP_DIR}" ]]; then
        "${BIN_DIR}/gnome-agent-status" completions fish > "${FISH_COMP_DIR}/gnome-agent-status.fish"
    else
        echo "    Note: ${FISH_COMP_DIR} is not writable in this environment; skipped fish completion."
    fi
fi

if "${BIN_DIR}/gnome-agent-status" manpage >/dev/null 2>&1; then
    MAN_DIR="${DATA_HOME}/man/man1"
    if mkdir -p "${MAN_DIR}" 2>/dev/null && [[ -w "${MAN_DIR}" ]]; then
        echo "==> Installing manual page to ${MAN_DIR}/gnome-agent-status.1..."
        "${BIN_DIR}/gnome-agent-status" manpage > "${MAN_DIR}/gnome-agent-status.1"
    fi
fi

if [[ -d "${SCRIPT_DIR}/contrib/systemd" ]]; then
    SYSTEMD_USER_DIR="${CONFIG_HOME}/systemd/user"
    if mkdir -p "${SYSTEMD_USER_DIR}" 2>/dev/null && [[ -w "${SYSTEMD_USER_DIR}" ]]; then
        echo "==> Installing systemd user units to ${SYSTEMD_USER_DIR}..."
        for unit in gnome-agent-status.service gnome-agent-status.timer; do
            if [[ -f "${SCRIPT_DIR}/contrib/systemd/${unit}" ]]; then
                install -m 0644 "${SCRIPT_DIR}/contrib/systemd/${unit}" "${SYSTEMD_USER_DIR}/${unit}"
            fi
        done
        if command -v systemctl >/dev/null 2>&1; then
            systemctl --user daemon-reload 2>/dev/null || true
            if [[ "${ENABLE_TIMER}" -eq 1 ]]; then
                echo "==> Enabling and starting gnome-agent-status.timer..."
                if ! systemctl --user enable --now gnome-agent-status.timer 2>/dev/null; then
                    echo "    Warning: systemctl --user could not enable gnome-agent-status.timer in this session."
                fi
            else
                echo "    Tip: Status-bar users outside GNOME Shell can enable periodic polling via:"
                echo "         systemctl --user enable --now gnome-agent-status.timer"
            fi
        elif [[ "${ENABLE_TIMER}" -eq 1 ]]; then
            echo "    Warning: systemctl not found; could not enable gnome-agent-status.timer."
        fi
    else
        echo "    Note: ${SYSTEMD_USER_DIR} is not writable in this environment; skipped systemd user units."
    fi
elif [[ "${ENABLE_TIMER}" -eq 1 ]]; then
    echo "    Note: contrib/systemd units not present in source tree; skipped --enable-timer."
fi

echo "==> Installing GNOME Shell extension to ${EXT_DEST}..."
rm -rf "${EXT_DEST}/popover" "${EXT_DEST}/icons" "${EXT_DEST}/schemas" "${EXT_DEST}/locale"
mkdir -p "${EXT_DEST}/popover" "${EXT_DEST}/icons" "${EXT_DEST}/schemas"

cp -f "${SCRIPT_DIR}/extension/metadata.json" "${EXT_DEST}/metadata.json"
cp -f "${SCRIPT_DIR}/extension/stylesheet.css" "${EXT_DEST}/stylesheet.css"
cp -f "${SCRIPT_DIR}"/extension/*.js "${EXT_DEST}/"
cp -f "${SCRIPT_DIR}"/extension/popover/*.js "${EXT_DEST}/popover/"
cp -f "${SCRIPT_DIR}"/extension/icons/*.svg "${EXT_DEST}/icons/"
cp -f "${SCRIPT_DIR}"/extension/schemas/*.gschema.xml "${EXT_DEST}/schemas/"

if compgen -G "${SCRIPT_DIR}/po/*.po" >/dev/null && command -v msgfmt >/dev/null 2>&1; then
    echo "==> Compiling gettext translation catalogs..."
    for po_file in "${SCRIPT_DIR}"/po/*.po; do
        lang="$(basename "${po_file}" .po)"
        mkdir -p "${EXT_DEST}/locale/${lang}/LC_MESSAGES"
        msgfmt -o "${EXT_DEST}/locale/${lang}/LC_MESSAGES/${EXT_UUID}.mo" "${po_file}"
    done
fi

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
