#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo_root/ci/workflow-test-helpers.sh"
ci_workflow="$repo_root/.github/workflows/ci.yml"
security_workflow="$repo_root/.github/workflows/security.yml"
permissions_policy="$repo_root/ci/workflow-permissions.json"

ci_json="$(workflow_yaml_to_json "$ci_workflow")"
job="$(jq -c '.jobs["gate-c-evidence"]' <<<"$ci_json")"

jq -e '
  .name == "Gate C endpoint evidence"
  and .if == "${{ github.event_name == '\''push'\'' }}"
  and .["runs-on"] == "ubuntu-latest"
  and .permissions == {"contents": "read"}
  and any(.steps[];
    .name == "Run revision-locked Gate C evidence"
    and (.run | contains("bash ci/run-gate-c-evidence.sh"))
    and (.run | contains("$GITHUB_SHA"))
    and (.run | contains("$GITHUB_RUN_ID"))
    and (.run | contains("$RUNNER_TEMP/gate-c-evidence")))
  and any(.steps[];
    .name == "Retain Gate C endpoint evidence"
    and .if == "${{ always() }}"
    and ((.uses // "") | startswith("actions/upload-artifact@"))
    and .with.name == "gate-c-endpoint-evidence"
    and .with.path == "${{ runner.temp }}/gate-c-evidence"
    and .with["if-no-files-found"] == "error")
' <<<"$job" >/dev/null

workflow_yaml_to_json "$security_workflow" | jq -e '
  .jobs["supply-chain"].steps[] |
  select(.run == "bash ci/test-gate-c-evidence-workflow.sh")
' >/dev/null

jq -e '
  .workflows[".github/workflows/ci.yml"]["gate-c-evidence"]
    == {"contents": "read"}
' "$permissions_policy" >/dev/null

echo 'Gate C evidence workflow contract verified'
