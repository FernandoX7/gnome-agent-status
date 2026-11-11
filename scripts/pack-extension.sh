#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
EXT_SRC="${REPO_ROOT}/extension"
EXT_UUID="agent-status@gnome-agent-status"
SCHEMA_REL="schemas/org.gnome.shell.extensions.agent-status.gschema.xml"
OUT_DIR="${OUT_DIR:-${REPO_ROOT}/target/extension-dist}"

usage() {
    cat <<'EOF'
Usage: ./scripts/pack-extension.sh [OPTIONS]

Validates GSettings schemas and builds an extensions.gnome.org (EGO) ready
.shell-extension.zip bundle.

Options:
  -o, --out-dir DIR  Output directory (default: ./target/extension-dist)
  -h, --help         Show this help message and exit
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -o|--out-dir)
            if [[ $# -lt 2 ]]; then
                echo "Error: $1 requires a directory argument" >&2
                exit 2
            fi
            OUT_DIR="$2"
            shift 2
            ;;
        --out-dir=*)
            OUT_DIR="${1#*=}"
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

if ! command -v glib-compile-schemas >/dev/null 2>&1; then
    echo "Error: glib-compile-schemas is required to validate ${SCHEMA_REL}" >&2
    exit 1
fi

echo "==> Validating GSettings schema with --strict..."
glib-compile-schemas --strict --dry-run "${EXT_SRC}/schemas"

mkdir -p "${OUT_DIR}"
OUT_DIR_ABS="$(cd "${OUT_DIR}" && pwd)"
ZIP_PATH="${OUT_DIR_ABS}/${EXT_UUID}.shell-extension.zip"

EXTRA_JS=()
for js_path in "${EXT_SRC}"/*.js; do
    js_base="$(basename "${js_path}")"
    if [[ "${js_base}" != "extension.js" && "${js_base}" != "prefs.js" ]]; then
        EXTRA_JS+=("${js_base}")
    fi
done

if command -v gnome-extensions >/dev/null 2>&1; then
    echo "==> Packing ${EXT_UUID} with gnome-extensions pack..."
    PACK_ARGS=(
        pack
        "${EXT_SRC}"
        --force
        "--out-dir=${OUT_DIR_ABS}"
        "--extra-source=popover"
        "--extra-source=icons"
        "--schema=${SCHEMA_REL}"
    )
    for js_file in "${EXTRA_JS[@]}"; do
        PACK_ARGS+=("--extra-source=${js_file}")
    done
    gnome-extensions "${PACK_ARGS[@]}"
elif command -v zip >/dev/null 2>&1; then
    echo "==> gnome-extensions not found; packing EGO archive via zip..."
    rm -f "${ZIP_PATH}"
    (
        cd "${EXT_SRC}"
        zip -q -r "${ZIP_PATH}" \
            metadata.json \
            stylesheet.css \
            extension.js \
            prefs.js \
            "${EXTRA_JS[@]}" \
            popover/*.js \
            icons/*.svg \
            "${SCHEMA_REL}" \
            -x "schemas/gschemas.compiled"
    )
else
    echo "Error: neither gnome-extensions nor zip is available to build ${ZIP_PATH}" >&2
    exit 1
fi

if [[ ! -f "${ZIP_PATH}" ]]; then
    echo "Error: expected archive not created at ${ZIP_PATH}" >&2
    exit 1
fi

echo "==> Verifying EGO bundle contents..."
if command -v unzip >/dev/null 2>&1; then
    ENTRIES="$(unzip -Z1 "${ZIP_PATH}")"
elif command -v python3 >/dev/null 2>&1; then
    ENTRIES="$(python3 -c 'import sys, zipfile; print("\n".join(zipfile.ZipFile(sys.argv[1]).namelist()))' "${ZIP_PATH}")"
else
    echo "Error: unzip or python3 is required to verify bundle contents" >&2
    exit 1
fi

require_entry() {
    local expected="$1"
    if ! grep -Fxq "${expected}" <<<"${ENTRIES}"; then
        echo "Error: archive missing required entry '${expected}'" >&2
        exit 1
    fi
}

require_entry "metadata.json"
require_entry "stylesheet.css"
require_entry "extension.js"
require_entry "prefs.js"
require_entry "${SCHEMA_REL}"

for js_file in "${EXTRA_JS[@]}"; do
    require_entry "${js_file}"
done

for pop_js in "${EXT_SRC}"/popover/*.js; do
    require_entry "popover/$(basename "${pop_js}")"
done

for svg_icon in "${EXT_SRC}"/icons/*.svg; do
    require_entry "icons/$(basename "${svg_icon}")"
done

if grep -Fxq "schemas/gschemas.compiled" <<<"${ENTRIES}"; then
    echo "Error: EGO archive must not include precompiled schemas/gschemas.compiled" >&2
    exit 1
fi

echo "==> EGO extension bundle verified: ${ZIP_PATH}"
