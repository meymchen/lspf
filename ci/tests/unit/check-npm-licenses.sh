#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT

allowed_file="$fixture_dir/allowed.txt"
lock_file="$fixture_dir/package-lock.json"
printf '%s\n' MIT Apache-2.0 >"$allowed_file"

write_lock() {
    local license="$1"
    jq -n --arg license "$license" '{
        lockfileVersion: 3,
        packages: {
            "": {name: "fixture"},
            "node_modules/example": {version: "1.0.0", license: $license}
        }
    }' >"$lock_file"
}

write_lock MIT
bash "$repo_root/ci/check-npm-licenses.sh" "$lock_file" "$allowed_file"

write_lock GPL-3.0-only
if output="$(bash "$repo_root/ci/check-npm-licenses.sh" "$lock_file" "$allowed_file" 2>&1)"; then
    echo 'expected an unapproved npm license to fail' >&2
    exit 1
fi
grep -F "node_modules/example@1.0.0 uses unapproved license 'GPL-3.0-only'" <<<"$output" >/dev/null

jq 'del(.packages["node_modules/example"].license)' "$lock_file" >"$lock_file.tmp"
mv "$lock_file.tmp" "$lock_file"
if output="$(bash "$repo_root/ci/check-npm-licenses.sh" "$lock_file" "$allowed_file" 2>&1)"; then
    echo 'expected an undeclared npm license to fail' >&2
    exit 1
fi
grep -F "node_modules/example@1.0.0 has no declared license" <<<"$output" >/dev/null

printf '%s\n' '{invalid json' >"$lock_file"
if output="$(bash "$repo_root/ci/check-npm-licenses.sh" "$lock_file" "$allowed_file" 2>&1)"; then
    echo 'expected a malformed npm lockfile to fail' >&2
    exit 1
fi
grep -F 'cannot parse npm lockfile' <<<"$output" >/dev/null

: >"$lock_file"
if output="$(bash "$repo_root/ci/check-npm-licenses.sh" "$lock_file" "$allowed_file" 2>&1)"; then
    echo 'expected an empty npm lockfile to fail' >&2
    exit 1
fi
grep -F 'cannot parse npm lockfile' <<<"$output" >/dev/null
