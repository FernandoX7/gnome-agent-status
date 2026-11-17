#!/usr/bin/env bash
set -euo pipefail

# Computes reachable CI job flags for pull requests while guaranteeing the full
# suite runs on push to main, merge_group, and workflow_dispatch.
# Fails closed (exit 1) if PR diff resolution fails or produces an empty file set,
# and enables all suites if an unrecognized path is modified.

EVENT_NAME="${EVENT_NAME:-push}"
BASE_SHA="${BASE_SHA:-}"
HEAD_SHA="${HEAD_SHA:-HEAD}"
OUTPUT_FILE="${GITHUB_OUTPUT:-}"

emit() {
    local key="$1"
    local val="$2"
    if [[ -n "${OUTPUT_FILE}" ]]; then
        printf '%s=%s\n' "${key}" "${val}" >> "${OUTPUT_FILE}"
    fi
    printf '%s=%s\n' "${key}" "${val}"
}

# Rule 1: Full suite runs unconditionally outside pull_request (main push, merge_group, manual).
if [[ "${EVENT_NAME}" != "pull_request" ]]; then
    emit "rust" "true"
    emit "audit" "true"
    emit "macos" "true"
    exit 0
fi

if [[ -z "${BASE_SHA}" ]] || ! git rev-parse --verify "${BASE_SHA}^{commit}" >/dev/null 2>&1; then
    echo "Error: BASE_SHA '${BASE_SHA}' is missing or not a valid commit; failing closed." >&2
    exit 1
fi

if ! git rev-parse --verify "${HEAD_SHA}^{commit}" >/dev/null 2>&1; then
    echo "Error: HEAD_SHA '${HEAD_SHA}' is not a valid commit; failing closed." >&2
    exit 1
fi

CHANGED_FILES="$(git diff --name-only "${BASE_SHA}...${HEAD_SHA}")"
if [[ -z "${CHANGED_FILES}" ]]; then
    echo "Error: git diff '${BASE_SHA}...${HEAD_SHA}' returned zero changed files; failing closed." >&2
    exit 1
fi

run_rust="false"
run_audit="false"
run_macos="false"

while IFS= read -r file; do
    [[ -z "${file}" ]] && continue

    case "${file}" in
        .github/workflows/ci.yml|scripts/ci-select.sh)
            run_rust="true"
            run_audit="true"
            run_macos="true"
            ;;
        Cargo.toml|Cargo.lock|deny.toml)
            run_rust="true"
            run_audit="true"
            run_macos="true"
            ;;
        .config/nextest.toml)
            run_rust="true"
            run_macos="true"
            ;;
        src/proc_exec.rs|src/cache.rs|src/config.rs|src/doctor.rs|src/integration_tests.rs|src/providers/mod.rs|src/providers/antigravity.rs|src/providers/cursor.rs|src/providers/windsurf.rs)
            run_rust="true"
            run_macos="true"
            ;;
        src/*.rs|src/**/*.rs)
            run_rust="true"
            if git diff -U0 "${BASE_SHA}...${HEAD_SHA}" -- "${file}" | grep -Eq '^[+-].*(cfg\(|target_os|unix|macos|Command|libc|std::os|permissions|symlink)'; then
                run_macos="true"
            fi
            ;;
        extension/*|po/*|tests/*|.oxlintrc.json|scripts/check-guardrails.sh|scripts/pack-extension.sh|scripts/run-oxlint.sh|scripts/install-hooks.sh|.githooks/*|install.sh|uninstall.sh)
            # Covered by extension-suite, which runs on every event.
            ;;
        README.md|CHANGELOG.md|CONTRIBUTING.md|SECURITY.md|LICENSE|.gitignore|config.example.toml|docs/*|contrib/*|.github/workflows/release.yml|.github/PULL_REQUEST_TEMPLATE.md|.github/ISSUE_TEMPLATE/*)
            # Pure documentation, templates, or release packaging metadata; guardrails + secret scan still run.
            ;;
        *)
            # Unclassified path: fail safe by enabling every suite so hidden changes cannot slip past PR CI.
            echo "Notice: unclassified path '${file}' enables full test matrix." >&2
            run_rust="true"
            run_audit="true"
            run_macos="true"
            ;;
    esac
done <<< "${CHANGED_FILES}"

emit "rust" "${run_rust}"
emit "audit" "${run_audit}"
emit "macos" "${run_macos}"
