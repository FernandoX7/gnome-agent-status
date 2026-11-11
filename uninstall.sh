#!/usr/bin/env bash
set -euo pipefail

EXT_UUID="agent-status@gnome-agent-status"
PURGE=0

usage() {
    cat <<'EOF'
Usage: ./uninstall.sh [OPTIONS]

Uninstalls the Agent Usage Status GNOME Shell extension and CLI binary.

Options:
  --purge     Also remove ~/.config/gnome-agent-status and ~/.cache/gnome-agent-status
  -h, --help  Show this help message and exit
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --purge)
            PURGE=1
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

BIN_DEST="${HOME}/.local/bin/gnome-agent-status"
DATA_HOME="${XDG_DATA_HOME:-${HOME}/.local/share}"
EXT_DEST="${DATA_HOME}/gnome-shell/extensions/${EXT_UUID}"
CONFIG_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/gnome-agent-status"
CACHE_DIR="${XDG_CACHE_HOME:-${HOME}/.cache}/gnome-agent-status"

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

remove_extension_gsettings() {
    local uuid="$1"
    if ! command -v gsettings >/dev/null 2>&1; then
        return 0
    fi

    local key current_list updated_list
    for key in enabled-extensions disabled-extensions; do
        if current_list="$(gsettings get org.gnome.shell "${key}" 2>/dev/null)"; then
            if [[ "${current_list}" == *"'${uuid}'"* ]]; then
                if updated_list="$(gvariant_str_array_remove "${current_list}" "${uuid}")"; then
                    if [[ "${updated_list}" != "${current_list}" ]]; then
                        gsettings set org.gnome.shell "${key}" "${updated_list}" 2>/dev/null || true
                    fi
                fi
            fi
        fi
    done
}

if [[ "${GSETTINGS_BACKEND:-}" != "memory" ]] && command -v gnome-extensions >/dev/null 2>&1; then
    if gnome-extensions info "${EXT_UUID}" >/dev/null 2>&1; then
        echo "==> Disabling extension ${EXT_UUID}..."
        gnome-extensions disable "${EXT_UUID}" 2>/dev/null || true
    fi
fi

echo "==> Removing ${EXT_UUID} from org.gnome.shell extension lists..."
remove_extension_gsettings "${EXT_UUID}"

if [[ -n "${EXT_DEST}" && "${EXT_DEST}" == *"/${EXT_UUID}" && -e "${EXT_DEST}" ]]; then
    echo "==> Removing extension directory ${EXT_DEST}..."
    rm -rf "${EXT_DEST}"
else
    echo "==> Extension directory not present (${EXT_DEST})."
fi

if [[ -n "${BIN_DEST}" && "${BIN_DEST}" == *"/gnome-agent-status" && -e "${BIN_DEST}" ]]; then
    echo "==> Removing binary ${BIN_DEST}..."
    rm -f "${BIN_DEST}"
else
    echo "==> Binary not present (${BIN_DEST})."
fi

if [[ "${PURGE}" -eq 1 ]]; then
    if [[ -n "${CONFIG_DIR}" && "${CONFIG_DIR}" == *"/gnome-agent-status" && -e "${CONFIG_DIR}" ]]; then
        echo "==> Purging configuration directory ${CONFIG_DIR}..."
        rm -rf "${CONFIG_DIR}"
    fi
    if [[ -n "${CACHE_DIR}" && "${CACHE_DIR}" == *"/gnome-agent-status" && -e "${CACHE_DIR}" ]]; then
        echo "==> Purging cache directory ${CACHE_DIR}..."
        rm -rf "${CACHE_DIR}"
    fi
else
    echo "    Note: Preserved ${CONFIG_DIR} and ${CACHE_DIR} (pass --purge to remove them)."
fi

echo "==> Uninstall complete."
