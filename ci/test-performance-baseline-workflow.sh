#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo_root/ci/workflow-test-helpers.sh"
ci_workflow="$repo_root/.github/workflows/ci.yml"
security_workflow="$repo_root/.github/workflows/security.yml"
permissions_policy="$repo_root/ci/workflow-permissions.json"

ci_json="$(workflow_yaml_to_json "$ci_workflow")"
job="$(jq -c '.jobs["performance-baseline"]' <<<"$ci_json")"

jq -e '
  .name == "Reproducible performance baseline"
  and .if == "${{ github.event_name == '\''push'\'' && needs.release-context.outputs.authorized == '\''true'\'' }}"
  and .needs == "release-context"
  and .["runs-on"] == "ubuntu-latest"
  and .permissions == {"contents": "read"}
  and any(.steps[];
    .name == "Test performance benchmark contract"
    and .run == "bash ci/test-performance-benchmark.sh")
  and any(.steps[];
    .name == "Test performance regression gate behavior"
    and .run == "bash ci/test-run-performance-baseline.sh")
  and any(.steps[];
    .name == "Run revision-locked performance baseline"
    and (.run | contains("bash ci/run-performance-baseline.sh"))
    and (.run | contains("$GITHUB_SHA"))
    and (.run | contains("$RUNNER_TEMP/performance-baseline")))
  and any(.steps[];
    .name == "Retain performance baseline"
    and .if == "${{ always() }}"
    and ((.uses // "") | startswith("actions/upload-artifact@"))
    and .with.name == "reproducible-performance-baseline"
    and .with.path == "${{ runner.temp }}/performance-baseline"
    and .with["if-no-files-found"] == "error")
' <<<"$job" >/dev/null

jq -e '
  (.jobs["gate-a-evidence"].needs | index("performance-baseline")) != null
' <<<"$ci_json" >/dev/null

workflow_yaml_to_json "$security_workflow" | jq -e '
  .jobs["supply-chain"].steps[] |
  select(.run == "bash ci/test-performance-baseline-workflow.sh")
' >/dev/null

jq -e '
  .workflows[".github/workflows/ci.yml"]["performance-baseline"]
    == {"contents": "read"}
' "$permissions_policy" >/dev/null

echo 'Performance baseline workflow contract verified'
