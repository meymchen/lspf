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

# `main` requires status checks. A path filter that skips this workflow leaves
# every required check pending rather than passing it vacuously, so a pull
# request touching only filtered paths can never merge without an admin bypass.
# The fast path belongs in job-level `if` conditions, which do report a result;
# it must never come back as a `pull_request` path filter.
#
# YAML 1.1 parsers read the `on` key as the boolean `true`, so accept both
# spellings: yq keeps `on`, while the `PyYAML` fallback yields `true`.
jq -e '
  (.on // .["true"]).pull_request // {}
  | (has("paths") or has("paths-ignore"))
  | not
' <<<"$ci_json" >/dev/null

for job in feature-matrix msrv native-matrix test-coverage performance-baseline \
    bounded-memory-soak gate-b-evidence gate-c-evidence
do
    jq -e --arg job "$job" '
      .jobs[$job].if == "${{ github.event_name == '\''push'\'' }}"
    ' <<<"$ci_json" >/dev/null
done

echo 'CI pull-request fast-path contract verified'
