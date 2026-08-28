#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo_root/ci/workflow-test-helpers.sh"

ci_json="$(workflow_yaml_to_json "$repo_root/.github/workflows/ci.yml")"
job="$(jq -c '.jobs["bounded-memory-soak"]' <<<"$ci_json")"

jq -e '
  .name == "Bounded-memory soak journeys (${{ matrix.scenario }})"
  and .if == "${{ github.event_name == '\''push'\'' }}"
  and .["runs-on"] == "ubuntu-latest"
  and .permissions == {"contents":"read"}
  and .strategy == {
    "fail-fast": false,
    "matrix": {
      "scenario": [
        "request",
        "cancellation",
        "edit",
        "progress",
        "slow-peer",
        "reconnect",
        "shutdown"
      ]
    }
  }
  and any(.steps[];
    .name == "Test soak runner contract"
    and .run == "bash ci/test-run-soak-journeys.sh")
  and any(.steps[];
    .name == "Test soak workload contract"
    and .run == "bash ci/test-soak-journeys.sh")
  and any(.steps[];
    .name == "Select soak scenario"
    and .env.SOAK_SCENARIO == "${{ matrix.scenario }}"
    and (.run | contains(".scenarios = [$scenario]"))
    and (.run | contains("$RUNNER_TEMP/soak-workload.json")))
  and any(.steps[];
    .name == "Run revision-locked soak journeys"
    and .env.SOAK_WORKLOADS == "${{ runner.temp }}/soak-workload.json"
    and (.run | contains("bash ci/run-soak-journeys.sh"))
    and (.run | contains("$GITHUB_SHA"))
    and (.run | contains("$RUNNER_TEMP/bounded-memory-soak")))
  and any(.steps[];
    .name == "Retain bounded-memory soak artifacts"
    and .if == "${{ always() }}"
    and ((.uses // "") | startswith("actions/upload-artifact@"))
    and .with.name == "bounded-memory-soak-${{ matrix.scenario }}"
    and .with.path == "${{ runner.temp }}/bounded-memory-soak"
    and .with["if-no-files-found"] == "error")
' <<<"$job" >/dev/null

jq -e '
  .workflows[".github/workflows/ci.yml"]["bounded-memory-soak"]
    == {"contents":"read"}
' "$repo_root/ci/workflow-permissions.json" >/dev/null

workflow_yaml_to_json "$repo_root/.github/workflows/security.yml" | jq -e '
  .jobs["supply-chain"].steps[]
  | select(.run == "bash ci/test-soak-journey-workflow.sh")
' >/dev/null

echo 'Soak journey workflow contract verified'
