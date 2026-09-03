#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

workloads="$test_root/workloads.json"
output="$test_root/results.json"
cat >"$workloads" <<'EOF'
{
  "schemaVersion": 1,
  "workloadVersion": 99,
  "workloads": {
    "startup": { "iterations": 2 },
    "requestLatency": { "warmupOperations": 1, "measuredOperations": 4 },
    "throughput": { "operations": 32 },
    "largeDocumentEditing": {
      "documentBytes": 4096,
      "edits": 4,
      "replacementBytes": 8
    },
    "notebookEditing": {
      "cells": 4,
      "edits": 4,
      "replacementBytes": 8
    },
    "partialResultThroughput": { "chunks": 32 },
    "slowPeer": {
      "attempts": 8,
      "outboundMessageLimit": 2,
      "outboundByteLimit": 65536,
      "writeDelayMs": 2
    }
  }
}
EOF

cargo bench -p lspf --bench performance_baseline --features testing -- \
    --workloads "$workloads" \
    --output "$output" \
    --revision 0123456789abcdef0123456789abcdef01234567

jq -e '
  .schemaVersion == 1
  and .workloadVersion == 99
  and .environmentMetadataVersion == 1
  and .revision == "0123456789abcdef0123456789abcdef01234567"
  and (.environment.os | length > 0)
  and (.environment.architecture | length > 0)
  and (.environment.logicalCpuCount >= 1)
  and (.environment.rustc | startswith("rustc "))
  and .environment.profile == "bench"
  and (.latencyMs.startupP95 >= 0)
  and (.latencyMs.requestP95 >= 0)
  and (.latencyMs.requestP99 >= .latencyMs.requestP95)
  and (.latencyMs.largeDocumentEditP95 >= 0)
  and (.latencyMs.largeDocumentEditP99 >= .latencyMs.largeDocumentEditP95)
  and (.latencyMs.notebookOpen >= 0)
  and (.latencyMs.notebookEditP95 >= 0)
  and (.latencyMs.notebookEditP99 >= .latencyMs.notebookEditP95)
  and (.throughputOperationsPerSecond > 0)
  and (.partialResultChunksPerSecond > 0)
  and (.peakRssMiB > 0)
  and (.limitBehavior.slowPeer.outboundMessageLimit == 2)
  and (.limitBehavior.slowPeer.attempted == 8)
  and (.limitBehavior.slowPeer.accepted
       + .limitBehavior.slowPeer.overloaded == 8)
  and (.limitBehavior.slowPeer.overloaded >= 1)
  and (.limitBehavior.slowPeer.delivered
       == .limitBehavior.slowPeer.accepted)
' "$output" >/dev/null

echo 'Performance benchmark verified'
