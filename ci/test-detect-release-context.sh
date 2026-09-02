#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT
head_revision="$(git rev-parse HEAD)"

RELEASE_TAG_EXISTS=true GITHUB_OUTPUT="$test_root/tagged.output" \
    bash ci/detect-release-context.sh meymchen/lspf main >/dev/null
grep -Fx 'pending=false' "$test_root/tagged.output" >/dev/null
grep -Fx 'authorized=false' "$test_root/tagged.output" >/dev/null

jq -n --arg revision "$head_revision" '[{
    number: 300,
    merged_at: "2026-09-02T10:00:00Z",
    title: "chore: release v99.99.99",
    merge_commit_sha: $revision,
    head: {
        ref: "release-plz-test",
        repo: {full_name: "meymchen/lspf"}
    }
}]' >"$test_root/authorized.json"

RELEASE_VERSION=99.99.99 \
RELEASE_TAG_EXISTS=false \
RELEASE_PRS_FILE="$test_root/authorized.json" \
GITHUB_OUTPUT="$test_root/authorized.output" \
    bash ci/detect-release-context.sh meymchen/lspf main >/dev/null
grep -Fx 'pending=true' "$test_root/authorized.output" >/dev/null
grep -Fx 'authorized=true' "$test_root/authorized.output" >/dev/null
grep -Fx 'pull-request=300' "$test_root/authorized.output" >/dev/null
grep -Fx 'version=99.99.99' "$test_root/authorized.output" >/dev/null

jq '.[0].head.ref = "feature/not-a-release"' \
    "$test_root/authorized.json" >"$test_root/unauthorized.json"
RELEASE_VERSION=99.99.99 \
RELEASE_TAG_EXISTS=false \
RELEASE_PRS_FILE="$test_root/unauthorized.json" \
GITHUB_OUTPUT="$test_root/unauthorized.output" \
    bash ci/detect-release-context.sh meymchen/lspf main >/dev/null
grep -Fx 'pending=true' "$test_root/unauthorized.output" >/dev/null
grep -Fx 'authorized=false' "$test_root/unauthorized.output" >/dev/null

echo 'Release context detection verified'
