#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

OXLINT_VERSION="1.94.0"
OXLINT_TAG="oxlint_v${OXLINT_VERSION}"

sha256_verify() {
    local file="$1"
    local expected="$2"
    local actual=""
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "${file}" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "${file}" | awk '{print $1}')"
    else
        echo "Error: neither sha256sum nor shasum is available to verify oxlint archive" >&2
        exit 1
    fi
    if [[ "${actual}" != "${expected}" ]]; then
        echo "Error: SHA-256 mismatch for ${file}: expected ${expected}, got ${actual}" >&2
        exit 1
    fi
}

resolve_oxlint_bin() {
    if command -v oxlint >/dev/null 2>&1; then
        local sys_ver
        sys_ver="$(oxlint --version 2>/dev/null || true)"
        if [[ "${sys_ver}" == *"${OXLINT_VERSION}"* ]]; then
            command -v oxlint
            return 0
        fi
    fi

    local cache_root="${OXLINT_CACHE_DIR:-${XDG_CACHE_HOME:-${HOME}/.cache}/gnome-agent-status-tools/oxlint-v${OXLINT_VERSION}}"
    local cached_bin="${cache_root}/oxlint"
    if [[ -x "${cached_bin}" ]]; then
        printf '%s\n' "${cached_bin}"
        return 0
    fi

    local os_name arch_name asset expected_sha
    os_name="$(uname -s)"
    arch_name="$(uname -m)"

    case "${os_name}:${arch_name}" in
        Linux:x86_64)
            asset="oxlint-x86_64-unknown-linux-gnu.tar.gz"
            expected_sha="fc1a189a35417ebda77cac83b8bfe9e9d10a7eb27e0d2fe005d222fa01261e64"
            ;;
        Linux:aarch64|Linux:arm64)
            asset="oxlint-aarch64-unknown-linux-gnu.tar.gz"
            expected_sha="ae4f0f826d3a96f529e197e460c9e4ffb4450528f55f4c688f6f6936f4660159"
            ;;
        Darwin:arm64|Darwin:aarch64)
            asset="oxlint-aarch64-apple-darwin.tar.gz"
            expected_sha="d31a59e3b11511075c13173f3c0b63fc0baa51477d73276dded7c308f4c26dc0"
            ;;
        Darwin:x86_64)
            asset="oxlint-x86_64-apple-darwin.tar.gz"
            expected_sha="7fe811a85916436e786836b84f9b976b824cdc050c54f9359dbc31f16cfc5f44"
            ;;
        *)
            echo "Error: unsupported platform ${os_name}:${arch_name} for prebuilt oxlint binary" >&2
            exit 1
            ;;
    esac

    local tmp_dir url archive_path
    tmp_dir="$(mktemp -d)"
    url="https://github.com/oxc-project/oxc/releases/download/${OXLINT_TAG}/${asset}"
    archive_path="${tmp_dir}/${asset}"

    curl -fsSL --proto '=https' --tlsv1.2 "${url}" -o "${archive_path}"
    sha256_verify "${archive_path}" "${expected_sha}"

    mkdir -p "${cache_root}"
    tar -xzf "${archive_path}" -C "${tmp_dir}"
    local extracted_bin
    extracted_bin="$(find "${tmp_dir}" -type f \( -name 'oxlint' -o -name 'oxlint-*' \) ! -name '*.tar.gz' | head -n 1)"
    if [[ -z "${extracted_bin}" ]]; then
        rm -rf "${tmp_dir}"
        echo "Error: oxlint binary not found inside ${asset}" >&2
        exit 1
    fi
    install -m 0755 "${extracted_bin}" "${cached_bin}.tmp.$$"
    mv -f "${cached_bin}.tmp.$$" "${cached_bin}"
    rm -rf "${tmp_dir}"

    printf '%s\n' "${cached_bin}"
}

OXLINT_BIN="$(resolve_oxlint_bin)"

if [[ $# -eq 0 ]]; then
    set -- extension/ tests/
fi

exec "${OXLINT_BIN}" -c "${REPO_ROOT}/.oxlintrc.json" "$@"
