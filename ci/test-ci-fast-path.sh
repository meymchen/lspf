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

jq -e '
  .jobs["commit-messages"] as $job
  | $job.name == "commit messages"
    and $job.if == "${{ github.event_name == '\''pull_request'\'' }}"
    and $job.permissions == {"contents": "read"}
    and ($job.steps | any(
      .uses == "actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803"
      and .with["fetch-depth"] == 0
      and .with["persist-credentials"] == false
    ))
    and ($job.steps | any(.run == "bash ci/test-check-commit-messages.sh"))
    and ($job.steps | any(
      .env.BASE_SHA == "${{ github.event.pull_request.base.sha }}"
      and .env.HEAD_SHA == "${{ github.event.pull_request.head.sha }}"
      and .run == "bash ci/check-commit-messages.sh \"$BASE_SHA\" \"$HEAD_SHA\""
    ))
' <<<"$ci_json" >/dev/null

echo 'CI pull-request fast-path contract verified'
