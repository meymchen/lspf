#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

source ci/test-coverage-schema.sh
source ci/test-coverage-cli.sh
source ci/test-coverage-report.sh

input_path=target/test-coverage/raw-summary.json
baseline_path=ci/policy/test-coverage-baseline.json
evidence_path=target/test-coverage/evidence.json
summary_path=target/test-coverage/summary.json

usage() {
    cat <<'EOF'
Usage: bash ci/check-test-coverage.sh [--input PATH] [--baseline PATH] [--evidence PATH] [--summary PATH]

Build a stable test-coverage summary from cargo-llvm-cov's JSON export and fail
if workspace coverage is not above the configured minimum, or if workspace or
protocol-engine coverage drops more than the configured regression tolerance.
Failure-path evidence must come from ci/run-coverage-evidence-tests.sh.
EOF
}

test_coverage_parse_options \
    test-coverage \
    usage \
    --input input_path \
    --baseline baseline_path \
    --evidence evidence_path \
    --summary summary_path \
    -- "$@"

test_coverage_report_bootstrap \
    summary_path \
    test-coverage \
    "the test-coverage gate did not complete"

[[ -f $input_path ]] || test_coverage_report_fail_setup "test-coverage export not found: $input_path"
[[ -f $baseline_path ]] || test_coverage_report_fail_setup "test-coverage baseline not found: $baseline_path"
[[ -f $evidence_path ]] || test_coverage_report_fail_setup "test evidence not found: $evidence_path"

if ! test_coverage_baseline_is_valid "$baseline_path" full; then
    test_coverage_report_fail_setup "invalid test-coverage baseline: $baseline_path"
fi

if ! jq -e --slurpfile baseline "$baseline_path" '
    .schemaVersion == 1
    and .success == true
    and (.tests | type == "object")
    and .tests == $baseline[0].evidence
' "$evidence_path" >/dev/null 2>&1; then
    test_coverage_report_fail_setup "test evidence does not match the declared failure-path tests"
fi

if ! jq -e '
    .type == "llvm.coverage.json.export"
    and (.data | type == "array" and length == 1)
    and (.data[0].totals.lines.count | type == "number")
    and (.data[0].totals.lines.covered | type == "number")
    and (.data[0].files | type == "array")
' "$input_path" >/dev/null 2>&1; then
    test_coverage_report_fail_setup "invalid cargo-llvm-cov JSON export: $input_path"
fi

jq -n \
    --slurpfile export "$input_path" \
    --slurpfile baseline "$baseline_path" \
    --slurpfile evidence "$evidence_path" '
  def percent($covered; $count):
    if $count == 0 then 100 else ((10000 * $covered / $count) | round) / 100 end;
  ($export[0].data[0]) as $data
  | ($baseline[0]) as $base
  | [
      $data.files[]
      | . as $file
      | $base.protocolEngineFiles[] as $path
      | select($file.filename == $path or ($file.filename | endswith("/" + $path)))
      | {path: $path, count: $file.summary.lines.count,
          covered: $file.summary.lines.covered}
    ] as $engineFiles
  | ($engineFiles | map(.count) | add // 0) as $engineCount
  | ($engineFiles | map(.covered) | add // 0) as $engineCovered
  | ($data.totals.lines) as $workspaceLines
  | percent($workspaceLines.covered; $workspaceLines.count) as $workspacePercent
  | percent($engineCovered; $engineCount) as $enginePercent
  | percent($base.thresholds.workspaceLines.covered;
      $base.thresholds.workspaceLines.count) as $workspaceBaselinePercent
  | percent($base.thresholds.protocolEngineLines.covered;
      $base.thresholds.protocolEngineLines.count) as $engineBaselinePercent
  | ($base.policy.minimumWorkspacePercentExclusive) as $workspaceMinimum
  | ($base.policy.maximumRegressionPercentagePoints) as $maximumDrop
  | ([
      if ($workspaceLines.covered * 100)
          <= ($workspaceMinimum * $workspaceLines.count) then
        {scope: "workspaceMinimum", actual: $workspacePercent,
          requiredExclusive: $workspaceMinimum}
      else empty end,
      if (100 * $workspaceLines.covered * $base.thresholds.workspaceLines.count)
          < ((100 * $base.thresholds.workspaceLines.covered
            - $maximumDrop * $base.thresholds.workspaceLines.count)
            * $workspaceLines.count) then
        {scope: "workspaceRegression", actual: $workspacePercent,
          baseline: $workspaceBaselinePercent, maximumDrop: $maximumDrop}
      else empty end,
      if $engineFiles | length != ($base.protocolEngineFiles | length) then
        {scope: "protocolEngine", error: "configured source file missing from export"}
      elif (100 * $engineCovered * $base.thresholds.protocolEngineLines.count)
          < ((100 * $base.thresholds.protocolEngineLines.covered
            - $maximumDrop * $base.thresholds.protocolEngineLines.count)
            * $engineCount) then
        {scope: "protocolEngineRegression", actual: $enginePercent,
          baseline: $engineBaselinePercent, maximumDrop: $maximumDrop}
      else empty end
    ]) as $failures
  | {
      schemaVersion: 1,
      success: ($failures | length == 0),
      policy: $base.policy,
      thresholds: $base.thresholds,
      testCoverage: {
        workspace: {lines: {count: $data.totals.lines.count,
          covered: $data.totals.lines.covered, percent: $workspacePercent}},
        protocolEngine: {
          files: ($engineFiles | map(.path)),
          lines: {count: $engineCount, covered: $engineCovered, percent: $enginePercent}
        }
      },
      evidence: $evidence[0].tests,
      failures: $failures
    }
' >"$summary_path"

if ! jq -e '.success' "$summary_path" >/dev/null; then
    jq -r '.failures[] |
      if .scope == "workspaceMinimum" then
        "test-coverage minimum: workspace \(.actual)% (required > \(.requiredExclusive)%)"
      elif (.scope | endswith("Regression")) then
        "test-coverage regression: \(.scope) \(.actual)% (baseline \(.baseline)%, maximum drop \(.maximumDrop) points)"
      else
        "test-coverage error: \(.scope) \(.error)"
      end' \
        "$summary_path" >&2
    exit 1
fi

jq -r '"test coverage passed: workspace \(.testCoverage.workspace.lines.percent)%, protocol engine \(.testCoverage.protocolEngine.lines.percent)%"' \
    "$summary_path"
