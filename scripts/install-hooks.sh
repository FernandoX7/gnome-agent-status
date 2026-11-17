#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

chmod 0755 "${REPO_ROOT}/.githooks/pre-push" \
    "${REPO_ROOT}/scripts/run-oxlint.sh" \
    "${REPO_ROOT}/scripts/ci-select.sh"

git -C "${REPO_ROOT}" config core.hooksPath .githooks
echo "Configured git core.hooksPath -> .githooks"
