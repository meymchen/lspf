#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

assert_classification() {
    local expected_build="$1"
    local expected_markdown="$2"
    shift 2

    printf '%s\n' "$@" >"$test_root/paths"
    CHANGED_FILES_FILE="$test_root/paths" GITHUB_OUTPUT="$test_root/output" \
        bash ci/classify-ci-changes.sh unused-base unused-head
    grep -Fx "build-checks=$expected_build" "$test_root/output" >/dev/null
    grep -Fx "markdown-checks=$expected_markdown" "$test_root/output" >/dev/null
    : >"$test_root/output"
}

assert_classification false true docs/adr/0021-ci.md AGENTS.md
assert_classification false false .vscode/settings.json .zed/settings.json
assert_classification true false crates/lspf/src/lib.rs
assert_classification true true README.md crates/lspf/src/lib.rs

echo 'CI change classification verified'
