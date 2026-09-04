#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/ci/tests/workflow-yaml.sh"
fuzz_workflow="$repo_root/.github/workflows/fuzz.yml"
security_workflow="$repo_root/.github/workflows/security.yml"
permissions_policy="$repo_root/ci/policy/workflow-permissions.json"

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

# The seven targets fan out as a matrix read from the runner itself, so the
# sweep costs one budget of wall clock rather than seven. What has to hold is
# that the list still comes from `ci/run-fuzz.sh` and that every leg's result
# reaches the job that assembles the evidence.
jq -e '
  .jobs["fuzz-matrix"] as $matrix
  | .jobs["fuzz-target"] as $target
  | $matrix != null
  and $target != null
  and $matrix.permissions == {"contents": "read"}
  and $matrix.outputs.matrix == "${{ steps.matrix.outputs.matrix }}"
  and any($matrix.steps[];
    .id == "matrix" and (.run | contains("ci/run-fuzz.sh --matrix")))
  and $target.needs == "fuzz-matrix"
  and $target.permissions == {"contents": "read"}
  and $target.strategy["fail-fast"] == false
  and $target.strategy.matrix
    == "${{ fromJSON(needs.fuzz-matrix.outputs.matrix) }}"
  and any($target.steps[];
    ((.uses // "") == "./.github/actions/setup-rust")
    and .with.toolchain == "nightly"
    and .with.cache == "false")
  and any($target.steps[];
    .name == "Install the fuzz runner"
    and .run == "cargo install cargo-fuzz --locked")
  and any($target.steps[];
    .name == "Fuzz one target"
    and .env.TARGET == "${{ matrix.target }}"
    and (.run | contains("ci/run-fuzz.sh --target")))
  and any($target.steps[];
    .name == "Retain the target result"
    and .if == "${{ always() }}"
    and ((.uses // "") | startswith("actions/upload-artifact@"))
    and .with.name == "fuzz-target-${{ matrix.target }}"
    and .with["if-no-files-found"] == "error"
    and .with["retention-days"] == 90)
' <<<"$fuzz_json" >/dev/null

jq -e '
  . != null
  and .name == "Gate D verification evidence"
  and .needs == ["fuzz-matrix", "fuzz-target"]
  and .if == "${{ !cancelled() }}"
  and .["runs-on"] == "ubuntu-latest"
  and .["timeout-minutes"] == 30
  and .permissions
    == {"actions": "read", "contents": "read", "issues": "write"}
  and any(.steps[];
    ((.uses // "") == "./.github/actions/setup-rust")
    and (.with.toolchain == null)
    and .with.cache == "false")
  and (any(.steps[]; .name == "Install the fuzz runner") | not)
  and any(.steps[];
    .name == "Download the fuzz target results"
    and (.run | contains("gh run download"))
    and (.run | contains("fuzz-target-")))
  and any(.steps[];
    .name == "Run revision-locked Gate D verification"
    and .env.GATE_D_FUZZ_RESULTS == "${{ runner.temp }}/fuzz-results"
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
  select(.run == "bash ci/tests/workflow/gate-d-evidence.sh")
' >/dev/null

jq -e '
  .workflows[".github/workflows/fuzz.yml"] == {
    "fuzz-matrix": {"contents": "read"},
    "fuzz-target": {"contents": "read"},
    "gate-d-evidence": {
      "actions": "read",
      "contents": "read",
      "issues": "write"
    }
  }
' "$permissions_policy" >/dev/null

echo 'Gate D evidence workflow contract verified'
