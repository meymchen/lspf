#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
checker="$repo_root/ci/check-editor-validation.sh"

bash "$checker" "$repo_root"

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT
cp -R "$repo_root/editor-validation" "$test_root/editor-validation"

rm "$test_root/editor-validation/neovim/init.lua"
if bash "$checker" "$test_root" >/dev/null 2>&1; then
    echo "checker accepted a journey without its Neovim configuration" >&2
    exit 1
fi

cp "$repo_root/editor-validation/neovim/init.lua" \
    "$test_root/editor-validation/neovim/init.lua"
jq '.humanUxObservations = .automatedEvidence | del(.automatedEvidence)' \
    "$repo_root/editor-validation/journeys-v1.json" \
    >"$test_root/editor-validation/journeys-v1.json"
if bash "$checker" "$test_root" >/dev/null 2>&1; then
    echo "checker accepted evidence with automated and human sections merged" >&2
    exit 1
fi

echo "editor validation contract tests passed"
