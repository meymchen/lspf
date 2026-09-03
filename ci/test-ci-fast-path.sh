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

build_condition="\${{ needs.release-context.outputs.build-checks == 'true' && (github.event_name != 'push' || needs.release-context.outputs.authorized == 'true') }}"
markdown_condition="\${{ needs.release-context.outputs.markdown-checks == 'true' && (github.event_name != 'push' || needs.release-context.outputs.authorized == 'true') }}"
release_condition="\${{ github.event_name == 'push' && needs.release-context.outputs.authorized == 'true' }}"

for job in feature-matrix fuzz-contract fmt public-docs packaged-crate security \
    public-api public-interface feature-contract test native-lifecycle wasm \
    test-coverage
do
    jq -e --arg job "$job" --arg condition "$build_condition" '
      .jobs[$job].if == $condition
      and (
        if (.jobs[$job].needs | type) == "array" then
          (.jobs[$job].needs | index("release-context")) != null
        else
          .jobs[$job].needs == "release-context"
        end
      )
    ' <<<"$ci_json" >/dev/null
done

for job in msrv native-matrix
do
    jq -e --arg job "$job" --arg condition "$build_condition" '
      .jobs[$job].if == $condition
      and (.jobs[$job].needs | sort) == ["feature-matrix", "release-context"]
    ' <<<"$ci_json" >/dev/null
done

jq -e --arg condition "$markdown_condition" '
  .jobs.markdownlint.if == $condition
  and .jobs.markdownlint.needs == "release-context"
' <<<"$ci_json" >/dev/null

for job in performance-baseline bounded-memory-soak gate-b-evidence gate-c-evidence
do
    jq -e --arg job "$job" --arg condition "$release_condition" '
      .jobs[$job].if == $condition
      and .jobs[$job].needs == "release-context"
    ' <<<"$ci_json" >/dev/null
done

jq -e '
  .jobs["release-context"] as $job
  | $job.name == "release context"
    and $job.permissions == {"contents": "read", "pull-requests": "read"}
    and $job.outputs.authorized == "${{ steps.release.outputs.authorized }}"
    and $job.outputs["build-checks"] == "${{ steps.changes.outputs.build-checks }}"
    and $job.outputs["markdown-checks"] == "${{ steps.changes.outputs.markdown-checks }}"
    and ($job.steps | any(
      .id == "changes"
      and .env.BASE_SHA == "${{ github.event.pull_request.base.sha }}"
      and .env.HEAD_SHA == "${{ github.event.pull_request.head.sha }}"
      and (.run | contains("bash ci/classify-ci-changes.sh"))
    ))
    and ($job.steps | any(
      .id == "release"
      and .env.GH_TOKEN == "${{ secrets.GITHUB_TOKEN }}"
      and (.run | contains("bash ci/detect-release-context.sh"))
    ))
' <<<"$ci_json" >/dev/null

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

jq -e '
  .jobs["merge-ready"] as $job
  | $job.name == "merge-ready"
    and $job.if == "${{ always() && github.event_name == '\''pull_request'\'' }}"
    and ($job.needs | sort) == [
      "commit-messages",
      "feature-contract",
      "feature-matrix",
      "fmt",
      "fuzz-contract",
      "markdownlint",
      "msrv",
      "native-lifecycle",
      "native-matrix",
      "packaged-crate",
      "public-api",
      "public-docs",
      "public-interface",
      "release-context",
      "security",
      "test",
      "test-coverage",
      "wasm"
    ]
    and $job.permissions == {"contents": "read"}
    and ($job.steps | any(
      .env.MERGE_READY_JOB_RESULTS == "${{ toJSON(needs) }}"
      and .env.BUILD_CHECKS == "${{ needs.release-context.outputs.build-checks }}"
      and .env.MARKDOWN_CHECKS == "${{ needs.release-context.outputs.markdown-checks }}"
      and .run == "bash ci/check-merge-ready.sh \"$GITHUB_EVENT_NAME\" \"$BUILD_CHECKS\" \"$MARKDOWN_CHECKS\""
    ))
' <<<"$ci_json" >/dev/null

jq -e '
  .jobs["release-plz-policy"] as $policy
  | .jobs["release-plz-pr"] as $release_pr
  | $policy.if == "${{ github.event_name == '\''push'\'' }}"
    and $policy.permissions == {"contents": "read"}
    and ($policy.steps | any(.run == "bash ci/check-workflow-security.sh"))
    and $release_pr.needs == "release-plz-policy"
' <<<"$ci_json" >/dev/null

echo 'CI pull-request fast-path contract verified'
