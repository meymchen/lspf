#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/ci/tests/workflow-yaml.sh"
ci_workflow="$repo_root/.github/workflows/ci.yml"
security_workflow="$repo_root/.github/workflows/security.yml"
permissions_policy="$repo_root/ci/policy/workflow-permissions.json"

ci_json="$(workflow_yaml_to_json "$ci_workflow")"
job="$(jq -c '.jobs["gate-a-evidence"]' <<<"$ci_json")"

jq -e '
  .name == "Gate A release evidence"
  and .if == "${{ always() && github.event_name == '\''push'\'' && needs.release-context.outputs.authorized == '\''true'\'' }}"
  and (.needs | sort) == [
    "feature-contract",
    "markdownlint",
    "msrv",
    "native-lifecycle",
    "native-matrix",
    "packaged-crate",
    "performance-baseline",
    "public-api",
    "public-docs",
    "public-interface",
    "release-context",
    "security",
    "test",
    "test-coverage",
    "wasm"
  ]
  and .permissions == {"contents": "read"}
  and any(.steps[];
    .name == "Assemble revision-locked Gate A evidence"
    and .env.GATE_A_JOB_RESULTS == "${{ toJSON(needs) }}"
    and (.run | contains("bash ci/prepare-gate-a-evidence.sh"))
    and (.run | contains("$GITHUB_SHA"))
    and (.run | contains("$GITHUB_RUN_ID")))
  and any(.steps[];
    .name == "Retain Gate A release evidence"
    and .if == "${{ always() }}"
    and ((.uses // "") | startswith("actions/upload-artifact@"))
    and .with.name == "gate-a-release-evidence"
    and .with.path == "target/gate-a-evidence"
    and .with["if-no-files-found"] == "error")
' <<<"$job" >/dev/null

workflow_yaml_to_json "$security_workflow" | jq -e '
  .jobs["supply-chain"].steps[] |
  select(.run == "bash ci/tests/workflow/gate-a-evidence.sh")
' >/dev/null

jq -e '
  .workflows[".github/workflows/ci.yml"]["gate-a-evidence"]
    == {"contents": "read"}
' "$permissions_policy" >/dev/null

echo "Gate A evidence workflow contract verified"
