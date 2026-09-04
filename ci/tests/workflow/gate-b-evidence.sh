#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/ci/tests/workflow-yaml.sh"
ci_workflow="$repo_root/.github/workflows/ci.yml"
security_workflow="$repo_root/.github/workflows/security.yml"
permissions_policy="$repo_root/ci/policy/workflow-permissions.json"

ci_json="$(workflow_yaml_to_json "$ci_workflow")"
job="$(jq -c '.jobs["gate-b-evidence"]' <<<"$ci_json")"

jq -e '
  .name == "Gate B bounded-resource evidence"
  and .if == "${{ github.event_name == '\''push'\'' && needs.release-context.outputs.authorized == '\''true'\'' }}"
  and .needs == "release-context"
  and .["runs-on"] == "ubuntu-latest"
  and .permissions == {"contents": "read"}
  and any(.steps[];
    .name == "Run revision-locked Gate B evidence"
    and (.run | contains("bash ci/run-gate-b-evidence.sh"))
    and (.run | contains("$GITHUB_SHA"))
    and (.run | contains("$GITHUB_RUN_ID"))
    and (.run | contains("$RUNNER_TEMP/gate-b-evidence")))
  and any(.steps[];
    .name == "Retain Gate B bounded-resource evidence"
    and .if == "${{ always() }}"
    and ((.uses // "") | startswith("actions/upload-artifact@"))
    and .with.name == "gate-b-bounded-resource-evidence"
    and .with.path == "${{ runner.temp }}/gate-b-evidence"
    and .with["if-no-files-found"] == "error")
' <<<"$job" >/dev/null

workflow_yaml_to_json "$security_workflow" | jq -e '
  .jobs["supply-chain"].steps[] |
  select(.run == "bash ci/tests/workflow/gate-b-evidence.sh")
' >/dev/null

jq -e '
  .workflows[".github/workflows/ci.yml"]["gate-b-evidence"]
    == {"contents": "read"}
' "$permissions_policy" >/dev/null

echo 'Gate B evidence workflow contract verified'
