#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

workloads="$test_root/workloads.json"
output="$test_root/results.json"
time_series="$test_root/time-series.jsonl"
cat >"$workloads" <<'EOF'
{
  "schemaVersion": 1,
  "workloadVersion": 99,
  "durationSeconds": 1,
  "sampleIntervalMilliseconds": 100,
  "scenarios": ["request","cancellation","edit","progress","slow-peer","reconnect","shutdown"],
  "traffic": {
    "requestConcurrency": 4,
    "cancellationConcurrency": 2,
    "editDocumentBytes": 4096,
    "progressConcurrency": 1,
    "slowPeerAttemptsPerCycle": 16,
    "reconnectsPerCycle": 2,
    "shutdownsPerCycle": 2
  },
  "limits": {
    "inboundRequests": 8,
    "outboundMessages": 4,
    "outboundBytes": 65536,
    "documents": 1,
    "documentBytes": 8192,
    "handlerTimeoutMilliseconds": 5000,
    "outboundRequestTimeoutMilliseconds": 5000
  }
}
EOF

cargo bench -p lspf --bench soak_journeys --features testing -- \
    --workloads "$workloads" \
    --output "$output" \
    --timeseries "$time_series" \
    --revision 0123456789abcdef0123456789abcdef01234567

jq -e '
  .schemaVersion == 1
  and .workloadVersion == 99
  and .revision == "0123456789abcdef0123456789abcdef01234567"
  and (.durationSeconds >= 7)
  and (.traffic.operations > 0)
  and (.traffic.bytes > 0)
  and (.peakRssMiB > 0)
  and (.unexplainedGrowthMiB >= 0)
  and ([.scenarios[].name] | sort) ==
      ["cancellation","edit","progress","reconnect","request","shutdown","slow-peer"]
  and all(.scenarios[];
    .result == "success"
    and .durationMilliseconds >= 1000
    and all(.terminalResources[]; . == 0))
' "$output" >/dev/null

if ! jq -s -e '
  length >= 14
  and ([.[].scenario] | unique | sort) ==
      ["cancellation","edit","progress","reconnect","request","shutdown","slow-peer"]
  and all(.[];
    (.elapsedMilliseconds | numbers)
    and (.rssMiB > 0)
    and (.resources | type == "object"))
  and any(.[]; .resources.inboundRequests > 0)
  and any(.[]; .resources.handlerTasks > 0)
  and any(.[]; .resources.documents > 0)
  and any(.[]; .resources.progressEntries > 0)
  and any(.[]; .resources.connections > 0)
  and any(.[]; .resources.outboundMessages > 0)
' "$time_series" >/dev/null
then
    jq -s '.' "$time_series" >&2
    exit 1
fi

echo 'Soak journey workload verified'

single_workload="$test_root/single-workload.json"
single_output="$test_root/single-results.json"
single_time_series="$test_root/single-time-series.jsonl"
jq '.scenarios = ["progress"]' "$workloads" >"$single_workload"

cargo bench -p lspf --bench soak_journeys --features testing -- \
    --workloads "$single_workload" \
    --output "$single_output" \
    --timeseries "$single_time_series" \
    --revision 0123456789abcdef0123456789abcdef01234567

jq -e '
  [.scenarios[].name] == ["progress"]
  and .scenarios[0].result == "success"
  and all(.scenarios[0].terminalResources[]; . == 0)
' "$single_output" >/dev/null

if ! jq -s -e '
  length >= 2
  and ([.[].scenario] | unique) == ["progress"]
' "$single_time_series" >/dev/null
then
    jq -s '.' "$single_time_series" >&2
    exit 1
fi

echo 'Single soak journey workload verified'
