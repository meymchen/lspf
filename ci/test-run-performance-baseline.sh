#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

workloads="$test_root/workloads.json"
budget="$test_root/budget.json"
cat >"$workloads" <<'EOF'
{
  "schemaVersion": 1,
  "workloadVersion": 7,
  "workloads": {
    "startup": { "iterations": 10 },
    "throughput": { "operations": 100 },
    "largeDocumentEditing": { "documentBytes": 1048576, "edits": 20 },
    "slowPeer": { "attempts": 16, "outboundMessageLimit": 4, "writeDelayMs": 5 }
  }
}
EOF
cat >"$budget" <<'EOF'
{
  "schemaVersion": 1,
  "workloadVersion": 7,
  "maximums": {
    "startupP95Ms": 25,
    "requestP95Ms": 5,
    "requestP99Ms": 10,
    "largeDocumentEditP95Ms": 20,
    "largeDocumentEditP99Ms": 30,
    "peakRssMiB": 128
  },
  "minimums": {
    "throughputOperationsPerSecond": 1000,
    "slowPeerOverloadCount": 1
  }
}
EOF

fake_cargo="$test_root/fake-cargo"
cat >"$fake_cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

output=
revision=
workloads=
while (($#)); do
    case "$1" in
        --output) output=$2; shift 2 ;;
        --revision) revision=$2; shift 2 ;;
        --workloads) workloads=$2; shift 2 ;;
        *) shift ;;
    esac
done

[[ $output == /* && $workloads == /* ]]
cat >"$output" <<JSON
{
  "schemaVersion": 1,
  "workloadVersion": 7,
  "environmentMetadataVersion": 1,
  "revision": "$revision",
  "environment": {
    "os": "linux",
    "architecture": "x86_64",
    "logicalCpuCount": 4,
    "rustc": "rustc test",
    "profile": "bench"
  },
  "latencyMs": {
    "startupP95": 12.5,
    "requestP95": 1.5,
    "requestP99": 2.5,
    "largeDocumentEditP95": 8.0,
    "largeDocumentEditP99": 9.0
  },
  "throughputOperationsPerSecond": 2500.0,
  "peakRssMiB": 64.0,
  "limitBehavior": {
    "slowPeer": {
      "outboundMessageLimit": 4,
      "attempted": 16,
      "accepted": 5,
      "overloaded": 11,
      "delivered": 5
    }
  }
}
JSON
EOF
chmod +x "$fake_cargo"

revision=0123456789abcdef0123456789abcdef01234567
output_dir="$test_root/report"
CARGO_BIN="$fake_cargo" \
    PERFORMANCE_WORKLOADS="$workloads" \
    PERFORMANCE_BUDGET="$budget" \
    bash ci/run-performance-baseline.sh "$revision" "$output_dir"

jq -e --arg revision "$revision" '
  .schemaVersion == 1
  and .workloadVersion == 7
  and .environmentMetadataVersion == 1
  and .revision == $revision
  and .overallResult == "success"
  and .environment.profile == "bench"
  and (.latencyMs.requestP99 == 2.5)
  and (.throughputOperationsPerSecond == 2500)
  and (.peakRssMiB == 64)
  and (.limitBehavior.slowPeer.overloaded == 11)
  and (.budgetChecks | length == 8)
  and all(.budgetChecks[]; .result == "success")
  and (.failedChecks | length == 0)
' "$output_dir/results.json" >/dev/null
cmp "$workloads" "$output_dir/workloads.json"
cmp "$budget" "$output_dir/regression-budget.json"

grep -F 'Overall result: **success**' "$output_dir/results.md" >/dev/null
grep -F '| Request p99 latency | 2.5 ms | 10 ms | success |' \
    "$output_dir/results.md" >/dev/null
grep -F '| Slow-peer overloads | 11 | at least 1 | success |' \
    "$output_dir/results.md" >/dev/null

failing_budget="$test_root/failing-budget.json"
jq '.maximums.requestP99Ms = 2' "$budget" >"$failing_budget"
failure_dir="$test_root/failure"
if CARGO_BIN="$fake_cargo" \
    PERFORMANCE_WORKLOADS="$workloads" \
    PERFORMANCE_BUDGET="$failing_budget" \
    bash ci/run-performance-baseline.sh "$revision" "$failure_dir"
then
    echo 'test failure: a budget regression produced a successful exit' >&2
    exit 1
fi

jq -e '
  .overallResult == "failure"
  and any(.failedChecks[];
    .id == "request-p99-latency" and .result == "failure")
' "$failure_dir/results.json" >/dev/null
grep -F 'Overall result: **failure**' "$failure_dir/results.md" >/dev/null

echo 'Performance baseline runner verified'
