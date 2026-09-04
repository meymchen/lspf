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

cp "$repo_root/editor-validation/journeys-v1.json" \
    "$test_root/editor-validation/journeys-v1.json"
cp -R "$repo_root/tools" "$test_root/tools"
cp -R "$repo_root/crates" "$test_root/crates"
bash "$checker" "$test_root" >/dev/null

# `recorded` is a self-assertion unless the worksheet it names ships with the
# repository, so a release record cannot cite human evidence nobody can open.
jq '.humanUxObservations.evidence = "editor-validation/observations/absent.md"' \
    "$repo_root/editor-validation/journeys-v1.json" \
    >"$test_root/editor-validation/journeys-v1.json"
if bash "$checker" "$test_root" >/dev/null 2>&1; then
    echo "checker accepted recorded observations without their archived worksheet" >&2
    exit 1
fi

jq 'del(.humanUxObservations.evidence)' \
    "$repo_root/editor-validation/journeys-v1.json" \
    >"$test_root/editor-validation/journeys-v1.json"
if bash "$checker" "$test_root" >/dev/null 2>&1; then
    echo "checker accepted recorded observations with no archived worksheet named" >&2
    exit 1
fi

jq '.humanUxObservations.evidence = "../../../etc/passwd"' \
    "$repo_root/editor-validation/journeys-v1.json" \
    >"$test_root/editor-validation/journeys-v1.json"
if bash "$checker" "$test_root" >/dev/null 2>&1; then
    echo "checker accepted an escaping observation worksheet path" >&2
    exit 1
fi

# A journey still under way records nothing and names no worksheet.
jq '.humanUxObservations.status = "pending"
    | .humanUxObservations.records = []
    | del(.humanUxObservations.evidence)' \
    "$repo_root/editor-validation/journeys-v1.json" \
    >"$test_root/editor-validation/journeys-v1.json"
bash "$checker" "$test_root" >/dev/null

echo "editor validation contract tests passed"
