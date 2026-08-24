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
  "thresholds": {
    "workspaceLines": {"count": 1000, "covered": 800},
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

write_export 86.5 81.0
run_gate
jq -e '
  .schemaVersion == 1
  and .success == true
  and .testCoverage.workspace.lines.percent == 86.5
  and .testCoverage.protocolEngine.lines.percent == 81
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

write_export 79.9 81.0
if run_gate; then
    echo 'test failure: workspace test-coverage regression passed the gate' >&2
    exit 1
fi
jq -e '
  .success == false
  and .failures == [{scope: "workspace", actual: 79.9, required: 80}]
' "$test_root/target/summary.json" >/dev/null

# The displayed percentage rounds to the baseline, but the exact ratio is
# lower. Cross-multiplication must still catch this otherwise-silent regression.
write_export 86.5 81.0
jq '.data[0].totals.lines = {count: 100001, covered: 80000, percent: 79.9992}' \
    "$test_root/target/test-coverage.json" \
    >"$test_root/target/test-coverage-within-rounding-bucket.json"
mv "$test_root/target/test-coverage-within-rounding-bucket.json" \
    "$test_root/target/test-coverage.json"
if run_gate; then
    echo 'test failure: a workspace ratio below the displayed baseline passed' >&2
    exit 1
fi
jq -e '
  .success == false
  and .failures == [{scope: "workspace", actual: 80, required: 80}]
' "$test_root/target/summary.json" >/dev/null

write_export 86.5 74.9
if run_gate; then
    echo 'test failure: protocol-engine test-coverage regression passed the gate' >&2
    exit 1
fi
jq -e '
  .success == false
  and .failures == [{scope: "protocolEngine", actual: 74.9, required: 75}]
' "$test_root/target/summary.json" >/dev/null

echo 'Test-coverage gate tests passed'
