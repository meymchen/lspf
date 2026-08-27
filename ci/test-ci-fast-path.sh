#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo_root/ci/workflow-test-helpers.sh"
ci_workflow="$repo_root/.github/workflows/ci.yml"

workflow_yaml_to_json "$ci_workflow" | jq -e '
  .concurrency.group
    == "${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}"
  and .concurrency["cancel-in-progress"]
    == "${{ github.event_name == '\''pull_request'\'' }}"
' >/dev/null

ci_json="$(workflow_yaml_to_json "$ci_workflow")"
for job in feature-matrix msrv native-matrix test-coverage gate-b-evidence gate-c-evidence
do
    jq -e --arg job "$job" '
      .jobs[$job].if == "${{ github.event_name == '\''push'\'' }}"
    ' <<<"$ci_json" >/dev/null
done

echo 'CI pull-request fast-path contract verified'
