#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT

mkdir -p "$test_root/ci" "$test_root/target"
cp ci/check-test-coverage.sh "$test_root/ci/"
cp ci/test-coverage-cli.sh "$test_root/ci/"
cp ci/test-coverage-report.sh "$test_root/ci/"
cp ci/test-coverage-schema.sh "$test_root/ci/"

cat >"$test_root/ci/test-coverage-baseline.json" <<'EOF'
{
  "schemaVersion": 1,
  "policy": {
    "minimumWorkspacePercentExclusive": 90,
    "maximumRegressionPercentagePoints": 5
  },
  "thresholds": {
    "workspaceLines": {"count": 1000, "covered": 950},
    "protocolEngineLines": {"count": 1000, "covered": 750}
  },
  "protocolEngineFiles": ["crates/lspf/src/engine.rs"],
  "evidence": {
    "lifecycle": [{
      "target": "lifecycle_hooks",
      "name": "failed_shutdown_hook_leaves_the_connection_running_for_retry"
    }],
    "cancellation": [{
      "target": "session_close",
      "name": "shutdown_answers_itself_then_cancels_and_refuses_later_work"
    }],
    "malformedMessage": [{
      "target": "lifecycle_hooks",
      "name": "malformed_shutdown_params_skip_the_hook_and_leave_the_connection_running"
    }],
    "close": [{
      "target": "session_close",
      "name": "writer_failure_closes_the_session_without_any_further_input"
    }]
  }
}
EOF

jq '{schemaVersion: 1, success: true, tests: .evidence}' \
    "$test_root/ci/test-coverage-baseline.json" \
    >"$test_root/target/test-evidence.json"

write_export() {
    local workspace_percent=$1
    local engine_percent=$2
    jq -n \
        --argjson workspacePercent "$workspace_percent" \
        --argjson enginePercent "$engine_percent" \
        '{type: "llvm.coverage.json.export", version: "2.0.1", data: [{
          files: [
            {filename: "/checkout/crates/lspf/src/engine.rs", summary: {lines: {
              count: 1000, covered: ($enginePercent * 10 | floor), percent: $enginePercent}}},
            {filename: "/checkout/crates/lspf/src/lib.rs", summary: {lines: {
              count: 100, covered: 95, percent: 95.0}}}
          ],
          totals: {lines: {count: 1000, covered: ($workspacePercent * 10 | floor),
            percent: $workspacePercent}}
        }]}' >"$test_root/target/test-coverage.json"
}

run_gate() {
    (
        cd "$test_root"
        bash ci/check-test-coverage.sh \
            --input target/test-coverage.json \
            --baseline ci/test-coverage-baseline.json \
            --evidence target/test-evidence.json \
            --summary target/summary.json
    )
}

write_export 90.1 70.0
run_gate
jq -e '
  .schemaVersion == 1
  and .success == true
  and .policy.minimumWorkspacePercentExclusive == 90
  and .policy.maximumRegressionPercentagePoints == 5
  and .testCoverage.workspace.lines.percent == 90.1
  and .testCoverage.protocolEngine.lines.percent == 70
  and .testCoverage.protocolEngine.files == ["crates/lspf/src/engine.rs"]
  and (.evidence | keys | sort
    == ["cancellation", "close", "lifecycle", "malformedMessage"])
' "$test_root/target/summary.json" >/dev/null

cp "$test_root/target/test-evidence.json" "$test_root/target/test-evidence-good.json"
jq 'del(.tests.lifecycle[0])' "$test_root/target/test-evidence-good.json" \
    >"$test_root/target/test-evidence.json"
if run_gate; then
    echo 'test failure: undeclared lifecycle test evidence passed the gate' >&2
    exit 1
fi
jq -e '
  .success == false
  and .setupError == "test evidence does not match the declared failure-path tests"
' "$test_root/target/summary.json" >/dev/null
mv "$test_root/target/test-evidence-good.json" "$test_root/target/test-evidence.json"

write_export 90.0 75.0
if run_gate; then
    echo 'test failure: the workspace minimum accepted exactly 90 percent' >&2
    exit 1
fi
jq -e '
  .success == false
  and .failures == [{scope: "workspaceMinimum", actual: 90, requiredExclusive: 90}]
' "$test_root/target/summary.json" >/dev/null

jq '.thresholds.workspaceLines = {count: 1000, covered: 970}' \
    "$test_root/ci/test-coverage-baseline.json" \
    >"$test_root/ci/test-coverage-baseline-updated.json"
mv "$test_root/ci/test-coverage-baseline-updated.json" \
    "$test_root/ci/test-coverage-baseline.json"
write_export 91.9 75.0
if run_gate; then
    echo 'test failure: a workspace regression over five points passed the gate' >&2
    exit 1
fi
jq -e '
  .success == false
  and .failures == [{scope: "workspaceRegression", actual: 91.9,
    baseline: 97, maximumDrop: 5}]
' "$test_root/target/summary.json" >/dev/null

write_export 92.0 69.9
if run_gate; then
    echo 'test failure: a protocol-engine regression over five points passed the gate' >&2
    exit 1
fi
jq -e '
  .success == false
  and .failures == [{scope: "protocolEngineRegression", actual: 69.9,
    baseline: 75, maximumDrop: 5}]
' "$test_root/target/summary.json" >/dev/null

echo 'Test-coverage gate tests passed'
