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

Validates GSettings schemas, compiles optional po/*.po catalogs into locale/,
and builds an extensions.gnome.org (EGO) ready .shell-extension.zip bundle.

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

STAGE_DIR="$(mktemp -d)"
trap 'rm -rf "${STAGE_DIR}"' EXIT

mkdir -p "${STAGE_DIR}/popover" "${STAGE_DIR}/icons" "${STAGE_DIR}/schemas"
cp -f "${EXT_SRC}/metadata.json" "${STAGE_DIR}/metadata.json"
cp -f "${EXT_SRC}/stylesheet.css" "${STAGE_DIR}/stylesheet.css"
cp -f "${EXT_SRC}"/*.js "${STAGE_DIR}/"
cp -f "${EXT_SRC}"/popover/*.js "${STAGE_DIR}/popover/"
cp -f "${EXT_SRC}"/icons/*.svg "${STAGE_DIR}/icons/"
cp -f "${EXT_SRC}/${SCHEMA_REL}" "${STAGE_DIR}/${SCHEMA_REL}"
rm -f "${STAGE_DIR}/schemas/gschemas.compiled"

EXTRA_JS=()
for js_path in "${EXT_SRC}"/*.js; do
    js_base="$(basename "${js_path}")"
    if [[ "${js_base}" != "extension.js" && "${js_base}" != "prefs.js" ]]; then
        EXTRA_JS+=("${js_base}")
    fi
done

compile_po_to_mo() {
    local po_in="$1"
    local mo_out="$2"
    if command -v msgfmt >/dev/null 2>&1; then
        msgfmt -o "${mo_out}" "${po_in}"
    elif command -v python3 >/dev/null 2>&1; then
        python3 - "${po_in}" "${mo_out}" <<'PY'
import ast, struct, sys

po_path, mo_path = sys.argv[1], sys.argv[2]
messages = {}
cur_id, cur_str, mode = None, None, None
fuzzy = False

def commit():
    global cur_id, cur_str, fuzzy
    if cur_id is not None and cur_str is not None and cur_str != "" and not fuzzy:
        messages[cur_id] = cur_str
    cur_id, cur_str, fuzzy = None, None, False

with open(po_path, "r", encoding="utf-8") as fh:
    for raw in fh:
        line = raw.strip()
        if not line:
            commit()
            mode = None
            continue
        if line.startswith("#,") and "fuzzy" in line:
            fuzzy = True
            continue
        if line.startswith("#"):
            continue
        if line.startswith("msgid "):
            commit()
            cur_id = ast.literal_eval(line[6:].strip())
            cur_str = ""
            mode = "id"
        elif line.startswith("msgstr "):
            cur_str = ast.literal_eval(line[7:].strip())
            mode = "str"
        elif line.startswith('"') and mode == "id":
            cur_id += ast.literal_eval(line)
        elif line.startswith('"') and mode == "str":
            cur_str += ast.literal_eval(line)
commit()

keys = sorted(messages.keys())
offsets = []
ids_blob = bytearray()
strs_blob = bytearray()
for k in keys:
    kb = k.encode("utf-8")
    vb = messages[k].encode("utf-8")
    offsets.append((len(ids_blob), len(kb), len(strs_blob), len(vb)))
    ids_blob += kb + b"\x00"
    strs_blob += vb + b"\x00"

n = len(keys)
keystart = 7 * 4 + 16 * n
valuestart = keystart + len(ids_blob)
koffsets, voffsets = [], []
for o1, l1, o2, l2 in offsets:
    koffsets += [l1, o1 + keystart]
    voffsets += [l2, o2 + valuestart]

header = struct.pack("Iiiiiii", 0x950412de, 0, n, 7 * 4, 7 * 4 + 8 * n, 0, 0)
table = struct.pack(f"{2 * n}i", *koffsets) + struct.pack(f"{2 * n}i", *voffsets)
with open(mo_path, "wb") as out:
    out.write(header + table + bytes(ids_blob) + bytes(strs_blob))
PY
    else
        echo "Error: msgfmt (gettext) or python3 is required to compile ${po_in}" >&2
        exit 1
    fi
}

shopt -s nullglob
PO_FILES=("${REPO_ROOT}"/po/*.po "${EXT_SRC}"/po/*.po)
shopt -u nullglob

EXPECTED_MO_ENTRIES=()
HAS_LOCALE=0
if (( ${#PO_FILES[@]} > 0 )); then
    echo "==> Compiling ${#PO_FILES[@]} translation catalog(s) into locale/..."
    for po_file in "${PO_FILES[@]}"; do
        lang="$(basename "${po_file}" .po)"
        mo_rel="locale/${lang}/LC_MESSAGES/${EXT_UUID}.mo"
        mkdir -p "${STAGE_DIR}/locale/${lang}/LC_MESSAGES"
        compile_po_to_mo "${po_file}" "${STAGE_DIR}/${mo_rel}"
        EXPECTED_MO_ENTRIES+=("${mo_rel}")
        HAS_LOCALE=1
    done
fi

if command -v gnome-extensions >/dev/null 2>&1; then
    echo "==> Packing ${EXT_UUID} with gnome-extensions pack..."
    PACK_ARGS=(
        pack
        "${STAGE_DIR}"
        --force
        "--out-dir=${OUT_DIR_ABS}"
        "--extra-source=popover"
        "--extra-source=icons"
        "--schema=${SCHEMA_REL}"
    )
    if [[ "${HAS_LOCALE}" -eq 1 ]]; then
        PACK_ARGS+=("--extra-source=locale")
    fi
    for js_file in "${EXTRA_JS[@]}"; do
        PACK_ARGS+=("--extra-source=${js_file}")
    done
    gnome-extensions "${PACK_ARGS[@]}"
elif command -v zip >/dev/null 2>&1; then
    echo "==> gnome-extensions not found; packing EGO archive via zip..."
    rm -f "${ZIP_PATH}"
    (
        cd "${STAGE_DIR}"
        ZIP_ITEMS=(
            metadata.json
            stylesheet.css
            extension.js
            prefs.js
            "${EXTRA_JS[@]}"
            popover/*.js
            icons/*.svg
            "${SCHEMA_REL}"
        )
        if [[ "${HAS_LOCALE}" -eq 1 ]]; then
            ZIP_ITEMS+=(locale)
        fi
        zip -q -r "${ZIP_PATH}" "${ZIP_ITEMS[@]}" -x "schemas/gschemas.compiled"
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

for mo_entry in "${EXPECTED_MO_ENTRIES[@]}"; do
    require_entry "${mo_entry}"
done

if grep -Fxq "schemas/gschemas.compiled" <<<"${ENTRIES}"; then
    echo "Error: EGO archive must not include precompiled schemas/gschemas.compiled" >&2
    exit 1
fi

echo "==> EGO extension bundle verified: ${ZIP_PATH}"
