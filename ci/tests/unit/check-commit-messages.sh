#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

git -C "$fixture" init --quiet
git -C "$fixture" config user.name 'Commit Policy Test'
git -C "$fixture" config user.email 'commit-policy@example.com'
git -C "$fixture" commit --quiet --allow-empty -m 'chore: establish test base'
base="$(git -C "$fixture" rev-parse HEAD)"

git -C "$fixture" commit --quiet --allow-empty -m 'fix: accept a concise subject'
head="$(git -C "$fixture" rev-parse HEAD)"

(
    cd "$fixture"
    bash "$repo_root/ci/check-commit-messages.sh" "$base" "$head"
)

git -C "$fixture" commit --quiet --allow-empty -m 'missing conventional type'
invalid="$(git -C "$fixture" rev-parse HEAD)"

if output="$(
    cd "$fixture"
    bash "$repo_root/ci/check-commit-messages.sh" "$base" "$invalid" 2>&1
)"; then
    printf 'expected a non-conventional subject to fail\n' >&2
    exit 1
fi

grep -F "${invalid:0:12}: subject is not a Conventional Commit" <<<"$output" >/dev/null

git -C "$fixture" reset --hard --quiet "$head"
body_500="$(printf 'a%.0s' {1..500})"
git -C "$fixture" commit --quiet --allow-empty \
    -m 'docs: accept the body length boundary' -m "$body_500"
boundary="$(git -C "$fixture" rev-parse HEAD)"

(
    cd "$fixture"
    bash "$repo_root/ci/check-commit-messages.sh" "$head" "$boundary"
)

body_501="${body_500}a"
git -C "$fixture" commit --quiet --allow-empty \
    -m 'docs: reject an oversized body' -m "$body_501"
oversized="$(git -C "$fixture" rev-parse HEAD)"

if output="$(
    cd "$fixture"
    bash "$repo_root/ci/check-commit-messages.sh" "$boundary" "$oversized" 2>&1
)"; then
    printf 'expected a 501-character body to fail\n' >&2
    exit 1
fi

grep -F "${oversized:0:12}: body has 501 characters; maximum is 500" \
    <<<"$output" >/dev/null

git -C "$fixture" reset --hard --quiet "$boundary"
body_490="$(printf 'b%.0s' {1..490})"
trailer_name="$(printf 'Contributor%.0s' {1..20})"
body_with_trailer="$(printf '%s\n\nCo-authored-by: %s <contributor@example.com>' \
    "$body_490" "$trailer_name")"
git -C "$fixture" commit --quiet --allow-empty \
    -m 'docs: exclude trailers from the body limit' -m "$body_with_trailer"
with_trailer="$(git -C "$fixture" rev-parse HEAD)"

(
    cd "$fixture"
    bash "$repo_root/ci/check-commit-messages.sh" "$boundary" "$with_trailer"
)

git -C "$fixture" commit --quiet --allow-empty \
    -m 'refactor!: reject a breaking commit without explanation'
breaking_without_body="$(git -C "$fixture" rev-parse HEAD)"

if output="$(
    cd "$fixture"
    bash "$repo_root/ci/check-commit-messages.sh" "$with_trailer" "$breaking_without_body" 2>&1
)"; then
    printf 'expected a breaking commit without a body to fail\n' >&2
    exit 1
fi

grep -F "${breaking_without_body:0:12}: breaking commit requires an explanatory body" \
    <<<"$output" >/dev/null

git -C "$fixture" reset --hard --quiet "$head"
git -C "$fixture" commit --quiet --allow-empty \
    -m 'docs: add the first aggregate body' -m "$body_500"
git -C "$fixture" commit --quiet --allow-empty \
    -m 'docs: reach the aggregate body boundary' -m "$body_500"
aggregate_boundary="$(git -C "$fixture" rev-parse HEAD)"

(
    cd "$fixture"
    bash "$repo_root/ci/check-commit-messages.sh" "$head" "$aggregate_boundary"
)

git -C "$fixture" commit --quiet --allow-empty \
    -m 'docs: exceed the aggregate body boundary' -m 'x'
aggregate_oversized="$(git -C "$fixture" rev-parse HEAD)"

if output="$(
    cd "$fixture"
    bash "$repo_root/ci/check-commit-messages.sh" "$head" "$aggregate_oversized" 2>&1
)"; then
    printf 'expected aggregate bodies over 1000 characters to fail\n' >&2
    exit 1
fi

grep -F 'commit body total is 1001 characters; maximum per pull request is 1000' \
    <<<"$output" >/dev/null

message_file="$fixture/COMMIT_EDITMSG"
printf 'refactor!: accept a concise breaking message\n\nExplain the migration.\n' \
    >"$message_file"
bash "$repo_root/ci/check-commit-messages.sh" --message-file "$message_file"

printf 'refactor!: reject a bodyless message\n' >"$message_file"
if output="$(
    bash "$repo_root/ci/check-commit-messages.sh" --message-file "$message_file" 2>&1
)"; then
    printf 'expected a bodyless breaking message file to fail\n' >&2
    exit 1
fi

grep -F 'COMMIT_EDITMSG: breaking commit requires an explanatory body' \
    <<<"$output" >/dev/null

if command -v prek >/dev/null; then
    mkdir -p "$fixture/ci"
    cp "$repo_root/prek.toml" "$fixture/prek.toml"
    cp "$repo_root/ci/check-commit-messages.sh" "$fixture/ci/check-commit-messages.sh"
    cp "$repo_root/ci/check-commit-messages.mjs" "$fixture/ci/check-commit-messages.mjs"
    git -C "$fixture" add prek.toml ci/check-commit-messages.sh \
        ci/check-commit-messages.mjs
    git -C "$fixture" commit --quiet -m 'test: install the commit message hook fixture'

    PREK_HOME="$fixture/prek-home" prek -C "$fixture" install
    test -f "$fixture/.git/hooks/pre-commit"
    test -f "$fixture/.git/hooks/commit-msg"

    if hook_output="$(
        PREK_HOME="$fixture/prek-home" git -C "$fixture" commit \
            --quiet --allow-empty -m 'missing conventional type' 2>&1
    )"; then
        printf 'expected the installed commit-msg hook to reject a commit\n' >&2
        exit 1
    fi
    grep -F 'subject is not a Conventional Commit' <<<"$hook_output" >/dev/null

    printf 'fix: validate through the local hook\n' >"$message_file"
    PREK_HOME="$fixture/prek-home" prek -C "$fixture" run \
        --stage commit-msg --commit-msg-filename "$message_file"
fi
