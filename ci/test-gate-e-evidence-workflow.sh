#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo_root/ci/workflow-test-helpers.sh"
ci_workflow="$repo_root/.github/workflows/ci.yml"
security_workflow="$repo_root/.github/workflows/security.yml"
permissions_policy="$repo_root/ci/workflow-permissions.json"

ci_json="$(workflow_yaml_to_json "$ci_workflow")"
job="$(jq -c '.jobs["gate-e-evidence"]' <<<"$ci_json")"

jq -e '
  .name == "Gate E candidate validation evidence"
  and .if == "${{ github.event_name == '\''push'\'' && needs.release-context.outputs.authorized == '\''true'\'' }}"
  and (.needs | sort) == ["release-candidate", "release-context"]
  and .["runs-on"] == "ubuntu-latest"
  and .permissions == {"actions": "read", "contents": "read"}
  and any(.steps[];
    ((.uses // "") | startswith("actions/checkout@"))
    and .with["fetch-depth"] == 0
    and .with["persist-credentials"] == false)
  and any(.steps[];
    .name == "Download the verified release candidate"
    and (.run | contains("gh run download"))
    and (.run | contains("$CANDIDATE_ARTIFACT"))
    and (.env.CANDIDATE_ARTIFACT
      | contains("needs.release-context.outputs.version"))
    and (.run | contains("$RUNNER_TEMP/candidate")))
  and any(.steps[];
    .name == "Validate the packaged candidate"
    and (.run | contains("bash ci/run-gate-e-evidence.sh"))
    and (.run | contains("$GITHUB_SHA"))
    and (.run | contains("$GITHUB_RUN_ID"))
    and (.run | contains("$RUNNER_TEMP/candidate"))
    and (.run | contains("$RUNNER_TEMP/gate-e-evidence")))
  and any(.steps[];
    .name == "Retain Gate E candidate validation evidence"
    and .if == "${{ always() }}"
    and ((.uses // "") | startswith("actions/upload-artifact@"))
    and .with.name == "gate-e-candidate-validation-evidence"
    and .with.path == "${{ runner.temp }}/gate-e-evidence"
    and .with["if-no-files-found"] == "error")
' <<<"$job" >/dev/null

# Gate E only means something after the candidate it validates exists.
jq -e '
  .jobs["release-candidate"].needs
  | index("gate-a-evidence") != null
  and index("gate-d-candidate-evidence") != null
' <<<"$ci_json" >/dev/null

workflow_yaml_to_json "$security_workflow" | jq -e '
  .jobs["supply-chain"].steps[] |
  select(.run == "bash ci/test-run-gate-e-evidence.sh")
' >/dev/null

workflow_yaml_to_json "$security_workflow" | jq -e '
  .jobs["supply-chain"].steps[] |
  select(.run == "bash ci/test-gate-e-evidence-workflow.sh")
' >/dev/null

jq -e '
  .workflows[".github/workflows/ci.yml"]["gate-e-evidence"]
    == {"actions": "read", "contents": "read"}
' "$permissions_policy" >/dev/null

echo 'Gate E evidence workflow contract verified'
