#!/usr/bin/env bash

set -euo pipefail

event_name="${1:?usage: check-merge-ready.sh EVENT BUILD_CHECKS MARKDOWN_CHECKS}"
build_checks="${2:?usage: check-merge-ready.sh EVENT BUILD_CHECKS MARKDOWN_CHECKS}"
markdown_checks="${3:?usage: check-merge-ready.sh EVENT BUILD_CHECKS MARKDOWN_CHECKS}"
job_results="${MERGE_READY_JOB_RESULTS:?MERGE_READY_JOB_RESULTS must contain the CI needs context as JSON}"

if ! jq -e 'type == "object"' <<<"$job_results" >/dev/null 2>&1; then
    echo 'MERGE_READY_JOB_RESULTS must be a JSON object' >&2
    exit 2
fi
if [[ $build_checks != true && $build_checks != false ]]; then
    printf 'BUILD_CHECKS must be true or false, found %s\n' "$build_checks" >&2
    exit 2
fi
if [[ $markdown_checks != true && $markdown_checks != false ]]; then
    printf 'MARKDOWN_CHECKS must be true or false, found %s\n' "$markdown_checks" >&2
    exit 2
fi

if [[ $event_name != pull_request ]]; then
    printf 'merge-ready does not support the %s event\n' "$event_name" >&2
    exit 2
fi

status=0
expect_result() {
    local job="$1"
    local expected="$2"
    local actual

    actual="$(jq -r --arg job "$job" '.[$job].result // "missing"' <<<"$job_results")"
    if [[ $actual != "$expected" ]]; then
        printf '%s must be %s for %s, found %s\n' \
            "$job" "$expected" "$event_name" "$actual" >&2
        status=1
    fi
}

expect_result commit-messages success
expect_result release-context success

if [[ $markdown_checks == true ]]; then
    expect_result markdownlint success
else
    expect_result markdownlint skipped
fi

build_result=skipped
if [[ $build_checks == true ]]; then
    build_result=success
fi

for job in feature-matrix fuzz-contract fmt public-docs packaged-crate security \
    public-api public-interface msrv feature-contract native-matrix test \
    native-lifecycle wasm test-coverage
do
    expect_result "$job" "$build_result"
done

exit "$status"
