#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

success_results="$({
    printf '%s\n' \
        commit-messages release-context markdownlint feature-matrix fuzz-contract fmt \
        public-docs packaged-crate security public-api public-interface msrv \
        feature-contract native-matrix test native-lifecycle wasm test-coverage
} | jq -Rn '[inputs | {key: ., value: {result: "success"}}] | from_entries')"

MERGE_READY_JOB_RESULTS="$success_results" \
    bash ci/check-merge-ready.sh pull_request true true

documentation_results="$(jq '
    .markdownlint.result = "success"
    | with_entries(
        if (.key == "commit-messages" or .key == "release-context" or .key == "markdownlint") then .
        else .value.result = "skipped"
        end
      )
' <<<"$success_results")"
MERGE_READY_JOB_RESULTS="$documentation_results" \
    bash ci/check-merge-ready.sh pull_request false true

if MERGE_READY_JOB_RESULTS="$(jq '.test.result = "failure"' <<<"$success_results")" \
    bash ci/check-merge-ready.sh pull_request true true
then
    echo 'expected a failed required job to reject merge readiness' >&2
    exit 1
fi

if MERGE_READY_JOB_RESULTS="$success_results" \
    bash ci/check-merge-ready.sh push true true
then
    echo 'expected an unsupported event to reject merge readiness' >&2
    exit 1
fi

echo 'Merge readiness aggregation verified'
