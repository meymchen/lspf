#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

workloads="$test_root/workloads.json"
thresholds="$test_root/thresholds.json"
cat >"$workloads" <<'EOF'
{
  "schemaVersion": 1,
  "workloadVersion": 7,
  "durationSeconds": 60,
  "sampleIntervalMilliseconds": 1000,
  "scenarios": ["request","cancellation","edit","progress","slow-peer","reconnect","shutdown"],
  "traffic": {
    "requestConcurrency": 8,
    "cancellationConcurrency": 4,
    "editDocumentBytes": 4096,
    "progressConcurrency": 2,
    "slowPeerAttemptsPerCycle": 16,
    "reconnectsPerCycle": 2,
    "shutdownsPerCycle": 2
  },
  "limits": {
    "inboundRequests": 8,
    "outboundMessages": 8,
    "outboundBytes": 65536,
    "documents": 2,
    "documentBytes": 8192,
    "handlerTimeoutMilliseconds": 5000,
    "outboundRequestTimeoutMilliseconds": 5000
  }
}
EOF
cat >"$thresholds" <<'EOF'
{
  "schemaVersion": 1,
  "workloadVersion": 7,
  "maximumRunSeconds": 600,
  "maximumPeakRssMiB": 256,
  "maximumUnexplainedGrowthMiB": 16,
  "minimumSamplesPerScenario": 2
}
EOF

fake_cargo="$test_root/fake-cargo"
cat >"$fake_cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

output=
timeseries=
revision=
while (($#)); do
    case "$1" in
        --output) output=$2; shift 2 ;;
        --timeseries) timeseries=$2; shift 2 ;;
        --revision) revision=$2; shift 2 ;;
        *) shift ;;
    esac
done

[[ $output == /* && $timeseries == /* ]]
for scenario in request cancellation edit progress slow-peer reconnect shutdown; do
    printf '{"scenario":"%s","elapsedMilliseconds":1000,"rssMiB":40,"resources":{"handlerTasks":1,"documents":0,"progressEntries":0,"connections":1,"outboundMessages":0}}\n' "$scenario" >>"$timeseries"
    printf '{"scenario":"%s","elapsedMilliseconds":2000,"rssMiB":41,"resources":{"handlerTasks":0,"documents":0,"progressEntries":0,"connections":0,"outboundMessages":0}}\n' "$scenario" >>"$timeseries"
done
cat >"$output" <<JSON
{
  "schemaVersion": 1,
  "workloadVersion": 7,
  "revision": "$revision",
  "durationSeconds": 420,
  "traffic": {"operations": 7000, "bytes": 8192},
  "peakRssMiB": 41,
  "unexplainedGrowthMiB": 1,
  "scenarios": [
    {"name":"request","result":"success","terminalOutcome":"exit","terminalResources":{"handlerTasks":0,"documents":0,"progressEntries":0,"connections":0,"outboundMessages":0}},
    {"name":"cancellation","result":"success","terminalOutcome":"exit","terminalResources":{"handlerTasks":0,"documents":0,"progressEntries":0,"connections":0,"outboundMessages":0}},
    {"name":"edit","result":"success","terminalOutcome":"exit","terminalResources":{"handlerTasks":0,"documents":0,"progressEntries":0,"connections":0,"outboundMessages":0}},
    {"name":"progress","result":"success","terminalOutcome":"exit","terminalResources":{"handlerTasks":0,"documents":0,"progressEntries":0,"connections":0,"outboundMessages":0}},
    {"name":"slow-peer","result":"success","terminalOutcome":"transport_closed","terminalResources":{"handlerTasks":0,"documents":0,"progressEntries":0,"connections":0,"outboundMessages":0}},
    {"name":"reconnect","result":"success","terminalOutcome":"exit","terminalResources":{"handlerTasks":0,"documents":0,"progressEntries":0,"connections":0,"outboundMessages":0}},
    {"name":"shutdown","result":"success","terminalOutcome":"exit","terminalResources":{"handlerTasks":0,"documents":0,"progressEntries":0,"connections":0,"outboundMessages":0}}
  ]
}
JSON
EOF
chmod +x "$fake_cargo"

revision=0123456789abcdef0123456789abcdef01234567
output_dir="$test_root/report"
CARGO_BIN="$fake_cargo" \
    SOAK_WORKLOADS="$workloads" \
    SOAK_THRESHOLDS="$thresholds" \
    bash ci/run-soak-journeys.sh "$revision" "$output_dir"

jq -e --arg revision "$revision" '
  .schemaVersion == 1
  and .workloadVersion == 7
  and .revision == $revision
  and .overallResult == "success"
  and .commandOutcome == "success"
  and (.timeSeries | length == 14)
  and ([.scenarios[].name] | sort) ==
      ["cancellation","edit","progress","reconnect","request","shutdown","slow-peer"]
  and all(.scenarios[];
    .result == "success"
    and all(.terminalResources[]; . == 0))
  and (.thresholdChecks | length == 3)
  and all(.thresholdChecks[]; .result == "success")
' "$output_dir/results.json" >/dev/null
cmp "$workloads" "$output_dir/workloads.json"
cmp "$thresholds" "$output_dir/thresholds.json"
grep -F 'Overall result: **success**' "$output_dir/results.md" >/dev/null
grep -F '| `slow-peer` | success | `transport_closed` |' "$output_dir/results.md" >/dev/null
grep -F '| Peak RSS | 41 MiB | at most 256 MiB | success |' "$output_dir/results.md" >/dev/null

retained_cargo="$test_root/retained-cargo"
sed 's/"handlerTasks":0/"handlerTasks":1/' "$fake_cargo" >"$retained_cargo"
chmod +x "$retained_cargo"
failure_dir="$test_root/failure"
if CARGO_BIN="$retained_cargo" \
    SOAK_WORKLOADS="$workloads" \
    SOAK_THRESHOLDS="$thresholds" \
    bash ci/run-soak-journeys.sh "$revision" "$failure_dir"
then
    echo 'test failure: retained handler tasks produced a successful exit' >&2
    exit 1
fi
jq -e '
  .overallResult == "failure"
  and any(.scenarios[];
    .result == "failure" and .terminalResources.handlerTasks == 1)
' "$failure_dir/results.json" >/dev/null

growth_cargo="$test_root/growth-cargo"
sed 's/"unexplainedGrowthMiB": 1/"unexplainedGrowthMiB": 17/' \
    "$fake_cargo" >"$growth_cargo"
chmod +x "$growth_cargo"
growth_dir="$test_root/growth"
if CARGO_BIN="$growth_cargo" \
    SOAK_WORKLOADS="$workloads" \
    SOAK_THRESHOLDS="$thresholds" \
    bash ci/run-soak-journeys.sh "$revision" "$growth_dir"
then
    echo 'test failure: unexplained memory growth produced a successful exit' >&2
    exit 1
fi
jq -e '
  .overallResult == "failure"
  and any(.failedChecks[]; .id == "unexplained-memory-growth")
' "$growth_dir/results.json" >/dev/null

crashing_cargo="$test_root/crashing-cargo"
cat >"$crashing_cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
timeseries=
while (($#)); do
    case "$1" in
        --timeseries) timeseries=$2; shift 2 ;;
        *) shift ;;
    esac
done
printf '%s\n' '{"scenario":"request","elapsedMilliseconds":1000,"rssMiB":40,"resources":{"inboundRequests":1,"handlerTasks":1,"documents":0,"progressEntries":0,"connections":1,"outboundMessages":0}}' >>"$timeseries"
exit 42
EOF
chmod +x "$crashing_cargo"
crash_dir="$test_root/crash"
if CARGO_BIN="$crashing_cargo" \
    SOAK_WORKLOADS="$workloads" \
    SOAK_THRESHOLDS="$thresholds" \
    bash ci/run-soak-journeys.sh "$revision" "$crash_dir"
then
    echo 'test failure: crashed soak process produced a successful exit' >&2
    exit 1
fi
jq -e '
  .overallResult == "failure"
  and .commandOutcome == "failure"
  and any(.scenarios[];
    .name == "request"
    and .result == "failure"
    and .terminalOutcome == "failure"
    and .terminalResources.handlerTasks == 1)
' "$crash_dir/results.json" >/dev/null

echo 'Soak journey runner verified'
