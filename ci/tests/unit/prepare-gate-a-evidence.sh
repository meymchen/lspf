#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

revision=0123456789abcdef0123456789abcdef01234567
run_url=https://github.com/meymchen/lspf/actions/runs/4242
output_dir="$test_root/evidence"

export GITHUB_REPOSITORY=meymchen/lspf
export GATE_A_JOB_RESULTS='{
  "markdownlint": {"result": "success"},
  "public-docs": {"result": "success"},
  "packaged-crate": {"result": "success"},
  "security": {"result": "success"},
  "public-api": {"result": "success"},
  "public-interface": {"result": "success"},
  "msrv": {"result": "success"},
  "feature-contract": {"result": "success"},
  "native-matrix": {"result": "success"},
  "test": {"result": "success"},
  "native-lifecycle": {"result": "success"},
  "wasm": {"result": "success"},
  "test-coverage": {"result": "success"}
}'

bash ci/prepare-gate-a-evidence.sh "$revision" "$run_url" "$output_dir"

jq -e \
  --arg revision "$revision" \
  --arg run "$run_url" '
    .schemaVersion == 1
    and .gate == "A"
    and .revision == $revision
    and .workflowRun == $run
    and .overallResult == "success"
    and ([.claims[].id] | sort) == [
      "compatibility-policy",
      "coverage",
      "documentation",
      "packaged-consumer",
      "release-traceability",
      "supply-chain",
      "support-matrix"
    ]
    and all(.claims[];
      .classification == "automated"
      and (.sources | length > 0)
      and all(.sources[]; contains("/blob/" + $revision + "/"))
      and (.checks | length > 0)
      and all(.checks[]; .result == "success" and .run == $run))
    and ([.artifacts[].name] | sort) == [
      "gate-a-release-evidence",
      "public-api-compatibility-report",
      "test-coverage-report"
    ]
    and all(.artifacts[]; .run == $run)
    and (.humanJudgments | length > 0)
    and all(.humanJudgments[];
      .classification == "human"
      and (.sources | length > 0)
      and all(.sources[]; contains("/blob/" + $revision + "/")))
  ' "$output_dir/evidence.json" >/dev/null

grep -F "Revision: [$revision](https://github.com/meymchen/lspf/commit/$revision)" \
  "$output_dir/evidence.md" >/dev/null
grep -F "Passing run: [CI run 4242]($run_url)" "$output_dir/evidence.md" >/dev/null
grep -F "## Human judgments" "$output_dir/evidence.md" >/dev/null

echo "Successful Gate A evidence generation verified"

failure_output_dir="$test_root/failing-evidence"
export GATE_A_JOB_RESULTS="$(jq '
  .test.result = "failure"
  | .["native-lifecycle"].result = "skipped"
' <<<"$GATE_A_JOB_RESULTS")"

if bash ci/prepare-gate-a-evidence.sh \
    "$revision" "$run_url" "$failure_output_dir"
then
    echo 'test failure: failing or skipped Gate A checks produced a successful exit' >&2
    exit 1
fi

jq -e '
  .overallResult == "failure"
  and ([.failedChecks[].id] | sort) == ["native-lifecycle", "test"]
  and all(.failedChecks[];
    (.result == "failure" or .result == "skipped")
    and (.explanation | type == "string" and length > 0))
  and ([.claims[].checks[] | select(.result != "success") | .id] | sort)
    == ([.failedChecks[].id] | sort)
' "$failure_output_dir/evidence.json" >/dev/null

grep -F '## Failing checks' "$failure_output_dir/evidence.md" >/dev/null
grep -F "Workflow run: [CI run 4242]($run_url)" \
  "$failure_output_dir/evidence.md" >/dev/null
if grep -F 'Passing run:' "$failure_output_dir/evidence.md" >/dev/null; then
    echo 'test failure: failing evidence labelled its workflow run as passing' >&2
    exit 1
fi
grep -F 'Workspace tests: `failure`' "$failure_output_dir/evidence.md" >/dev/null
grep -F 'Cross-platform native lifecycle: `skipped`' \
  "$failure_output_dir/evidence.md" >/dev/null

echo "Failing Gate A evidence generation verified"

missing_output_dir="$test_root/missing-evidence"
export GATE_A_JOB_RESULTS="$(jq 'del(.wasm)' <<<"$GATE_A_JOB_RESULTS")"

if bash ci/prepare-gate-a-evidence.sh \
    "$revision" "$run_url" "$missing_output_dir"
then
    echo 'test failure: an absent Gate A result produced a successful exit' >&2
    exit 1
fi

jq -e '
  .overallResult == "failure"
  and any(.failedChecks[];
    .id == "wasm"
    and .result == "missing"
    and (.explanation | contains("did not report")))
' "$missing_output_dir/evidence.json" >/dev/null

echo "Missing Gate A evidence result verified"
