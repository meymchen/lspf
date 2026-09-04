#!/usr/bin/env bash
set -euo pipefail

lock_file="${1:-tools/vscode-test-client/package-lock.json}"
allowed_file="${2:-ci/policy/npm-allowed-licenses.txt}"
status=0
dependencies=""
dependency_rows=""

if ! command -v jq >/dev/null; then
    echo 'required command is unavailable: jq' >&2
    exit 2
fi

if ! dependencies="$(jq -e -c '.packages |
    if type == "object" then
        [to_entries[] | select(.key != "") |
            [.key, (.value.version // "UNKNOWN"), (.value.license // "UNDECLARED")]]
    else
        error("packages must be an object")
    end' "$lock_file")"; then
    printf 'cannot parse npm lockfile: %s\n' "$lock_file" >&2
    exit 2
fi

if ! dependency_rows="$(jq -r '.[] | @tsv' <<<"$dependencies")"; then
    printf 'cannot inspect npm lockfile: %s\n' "$lock_file" >&2
    exit 2
fi

if [[ -n "$dependency_rows" ]]; then
    while IFS=$'\t' read -r package version license; do
        if [[ "$license" == UNDECLARED ]]; then
            printf '%s@%s has no declared license\n' "$package" "$version" >&2
            status=1
        elif ! grep -Fx -- "$license" "$allowed_file" >/dev/null; then
            printf "%s@%s uses unapproved license '%s'\n" \
                "$package" "$version" "$license" >&2
            status=1
        fi
    done <<<"$dependency_rows"
fi

exit "$status"
