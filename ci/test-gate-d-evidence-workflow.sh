#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo_root/ci/workflow-test-helpers.sh"
fuzz_workflow="$repo_root/.github/workflows/fuzz.yml"
security_workflow="$repo_root/.github/workflows/security.yml"
permissions_policy="$repo_root/ci/workflow-permissions.json"

fuzz_json="$(workflow_yaml_to_json "$fuzz_workflow")"
job="$(jq -c '.jobs["gate-d-evidence"]' <<<"$fuzz_json")"
if [[ $job == null ]]; then
    echo 'Gate D evidence job is missing' >&2
    exit 1
fi

# Gate D runs on a schedule rather than per push. The cadence itself is a tuning
# knob, so this asserts that both triggers exist without pinning the cron
# expression; the retention and permission assertions below are the contract.
#
# The trigger block is read as `.on // .["true"]` because the two backends of
# `workflow_yaml_to_json` disagree about this one key: yq follows YAML 1.2 and
# keeps `on` a string, while PyYAML follows YAML 1.1 and resolves it to the
# boolean `true`.
jq -e '
  (.on // .["true"]) as $triggers
  | ($triggers.schedule | type == "array" and length > 0)
    and ($triggers | has("workflow_dispatch"))
' <<<"$fuzz_json" >/dev/null

jq -e '
  . != null
  and .name == "Gate D verification evidence"
  and (.if == null)
  and .["runs-on"] == "ubuntu-latest"
  and .["timeout-minutes"] == 60
  and .permissions == {"contents": "read", "issues": "write"}
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
  and any(.steps[];
    .name == "Report the failure once"
    and .if == "${{ failure() }}"
    and (.run | contains("gh issue list --label gate-d-failure"))
    and (.run | contains("gh issue create")))
' <<<"$job" >/dev/null

workflow_yaml_to_json "$security_workflow" | jq -e '
  .jobs["supply-chain"].steps[] |
  select(.run == "bash ci/test-gate-d-evidence-workflow.sh")
' >/dev/null

jq -e '
  .workflows[".github/workflows/fuzz.yml"]["gate-d-evidence"]
    == {"contents": "read", "issues": "write"}
' "$permissions_policy" >/dev/null

echo 'Gate D evidence workflow contract verified'
