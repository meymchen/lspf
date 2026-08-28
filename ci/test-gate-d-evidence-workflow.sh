#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo_root/ci/workflow-test-helpers.sh"
ci_workflow="$repo_root/.github/workflows/ci.yml"
security_workflow="$repo_root/.github/workflows/security.yml"
permissions_policy="$repo_root/ci/workflow-permissions.json"

ci_json="$(workflow_yaml_to_json "$ci_workflow")"
job="$(jq -c '.jobs["gate-d-evidence"]' <<<"$ci_json")"
if [[ $job == null ]]; then
    echo 'Gate D evidence job is missing' >&2
    exit 1
fi

jq -e '
  . != null
  and .name == "Gate D verification evidence"
  and .if == "${{ github.event_name == '\''push'\'' }}"
  and .["runs-on"] == "ubuntu-latest"
  and .["timeout-minutes"] == 60
  and .permissions == {"contents": "read"}
  and any(.steps[];
    ((.uses // "") == "./.github/actions/setup-rust")
    and .with.toolchain == "nightly"
    and .with.cache == "false")
  and any(.steps[];
    .name == "Install the fuzz runner"
    and .run == "cargo install cargo-fuzz --locked")
  and any(.steps[];
    .name == "Run revision-locked Gate D verification"
    and (.run | contains("bash ci/run-gate-d-evidence.sh"))
    and (.run | contains("$GITHUB_SHA"))
    and (.run | contains("$GITHUB_RUN_ID"))
    and (.run | contains("$RUNNER_TEMP/gate-d-evidence")))
  and any(.steps[];
    .name == "Retain Gate D verification evidence"
    and .if == "${{ always() }}"
    and ((.uses // "") | startswith("actions/upload-artifact@"))
    and .with.name == "gate-d-verification-evidence"
    and .with.path == "${{ runner.temp }}/gate-d-evidence"
    and .with["if-no-files-found"] == "error"
    and .with["retention-days"] == 90)
' <<<"$job" >/dev/null

workflow_yaml_to_json "$security_workflow" | jq -e '
  .jobs["supply-chain"].steps[] |
  select(.run == "bash ci/test-gate-d-evidence-workflow.sh")
' >/dev/null

jq -e '
  .workflows[".github/workflows/ci.yml"]["gate-d-evidence"]
    == {"contents": "read"}
' "$permissions_policy" >/dev/null

echo 'Gate D evidence workflow contract verified'
