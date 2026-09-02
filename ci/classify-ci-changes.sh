#!/usr/bin/env bash

set -euo pipefail

base_revision="${1:?usage: classify-ci-changes.sh BASE_REVISION HEAD_REVISION}"
head_revision="${2:?usage: classify-ci-changes.sh BASE_REVISION HEAD_REVISION}"
output="${GITHUB_OUTPUT:?GITHUB_OUTPUT must name the workflow output file}"

build_checks=false
markdown_checks=false

classify_path() {
    local path="$1"

    if [[ $path == *.md ]]; then
        markdown_checks=true
    fi

    case "$path" in
        docs/adr/*|docs/agents/*|.vscode/*|.zed/*|CONTEXT.md|AGENTS.md|CLAUDE.md)
            ;;
        *)
            build_checks=true
            ;;
    esac
}

if [[ -n ${CHANGED_FILES_FILE:-} ]]; then
    while IFS= read -r path || [[ -n $path ]]; do
        classify_path "$path"
    done <"$CHANGED_FILES_FILE"
else
    while IFS= read -r -d '' path; do
        classify_path "$path"
    done < <(git diff --name-only -z "$base_revision" "$head_revision" --)
fi

{
    printf 'build-checks=%s\n' "$build_checks"
    printf 'markdown-checks=%s\n' "$markdown_checks"
} >>"$output"
