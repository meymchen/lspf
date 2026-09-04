#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

revision="${1:?usage: run-performance-baseline.sh <40-character revision> <output-directory>}"
output_dir="${2:?usage: run-performance-baseline.sh <40-character revision> <output-directory>}"

if [[ ! $revision =~ ^[0-9a-f]{40}$ ]]; then
    echo 'Performance baseline revision must be a 40-character lowercase hexadecimal SHA' >&2
    exit 2
fi

workloads="$(realpath -m "${PERFORMANCE_WORKLOADS:-performance/workloads-v2.json}")"
budget="$(realpath -m "${PERFORMANCE_BUDGET:-performance/regression-budget-v2.json}")"
output_dir="$(realpath -m "$output_dir")"
raw_results="$output_dir/raw-results.json"
cargo_bin="${CARGO_BIN:-cargo}"

for input in "$workloads" "$budget"; do
    if [[ ! -f $input ]]; then
        echo "Performance baseline input does not exist: $input" >&2
        exit 2
    fi
done

workload_version="$(jq -er '.workloadVersion | numbers' "$workloads")"
budget_workload_version="$(jq -er '.workloadVersion | numbers' "$budget")"
if [[ $workload_version != "$budget_workload_version" ]]; then
    echo 'Performance workload and regression budget versions do not match' >&2
    exit 2
fi

mkdir -p "$output_dir"
cp "$workloads" "$output_dir/workloads.json"
cp "$budget" "$output_dir/regression-budget.json"
"$cargo_bin" bench -p lspf --bench performance_baseline --features testing -- \
    --workloads "$workloads" \
    --output "$raw_results" \
    --revision "$revision"

jq -e \
    --arg revision "$revision" \
    --argjson workload_version "$workload_version" '
      .schemaVersion == 1
      and .workloadVersion == $workload_version
      and .environmentMetadataVersion == 1
      and .revision == $revision
      and (.environment | type == "object")
      and (.latencyMs | type == "object")
      and (.latencyMs.notebookOpen | numbers)
      and (.latencyMs.notebookEditP95 | numbers)
      and (.latencyMs.notebookEditP99 | numbers)
      and (.throughputOperationsPerSecond | numbers)
      and (.partialResultChunksPerSecond | numbers)
      and (.peakRssMiB | numbers)
      and (.limitBehavior.slowPeer | type == "object")
    ' "$raw_results" >/dev/null

jq --slurpfile budget "$budget" '
  def maximum($id; $name; $actual; $maximum; $unit):
    {
      id: $id,
      name: $name,
      actual: $actual,
      budget: $maximum,
      comparison: "maximum",
      unit: $unit,
      result: (if $actual <= $maximum then "success" else "failure" end)
    };
  def minimum($id; $name; $actual; $minimum; $unit):
    {
      id: $id,
      name: $name,
      actual: $actual,
      budget: $minimum,
      comparison: "minimum",
      unit: $unit,
      result: (if $actual >= $minimum then "success" else "failure" end)
    };
  . as $results
  | $budget[0] as $limits
  | .budgetChecks = [
      maximum("startup-p95-latency"; "Startup p95 latency";
        $results.latencyMs.startupP95; $limits.maximums.startupP95Ms; "ms"),
      maximum("request-p95-latency"; "Request p95 latency";
        $results.latencyMs.requestP95; $limits.maximums.requestP95Ms; "ms"),
      maximum("request-p99-latency"; "Request p99 latency";
        $results.latencyMs.requestP99; $limits.maximums.requestP99Ms; "ms"),
      maximum("large-document-edit-p95-latency"; "Large-document edit p95 latency";
        $results.latencyMs.largeDocumentEditP95;
        $limits.maximums.largeDocumentEditP95Ms; "ms"),
      maximum("large-document-edit-p99-latency"; "Large-document edit p99 latency";
        $results.latencyMs.largeDocumentEditP99;
        $limits.maximums.largeDocumentEditP99Ms; "ms"),
      maximum("notebook-open-latency"; "Notebook open latency";
        $results.latencyMs.notebookOpen;
        $limits.maximums.notebookOpenMs; "ms"),
      maximum("notebook-edit-p95-latency"; "Notebook edit p95 latency";
        $results.latencyMs.notebookEditP95;
        $limits.maximums.notebookEditP95Ms; "ms"),
      maximum("notebook-edit-p99-latency"; "Notebook edit p99 latency";
        $results.latencyMs.notebookEditP99;
        $limits.maximums.notebookEditP99Ms; "ms"),
      maximum("peak-rss"; "Peak RSS";
        $results.peakRssMiB; $limits.maximums.peakRssMiB; "MiB"),
      minimum("throughput"; "Throughput";
        $results.throughputOperationsPerSecond;
        $limits.minimums.throughputOperationsPerSecond; "operations/s"),
      minimum("partial-result-throughput"; "Partial-result chunk throughput";
        $results.partialResultChunksPerSecond;
        $limits.minimums.partialResultChunksPerSecond; "chunks/s"),
      minimum("slow-peer-overloads"; "Slow-peer overloads";
        $results.limitBehavior.slowPeer.overloaded;
        $limits.minimums.slowPeerOverloadCount; "count")
    ]
  | .failedChecks = [.budgetChecks[] | select(.result != "success")]
  | .overallResult =
      (if (.failedChecks | length) == 0 then "success" else "failure" end)
' "$raw_results" >"$output_dir/results.json"

jq -r '
  # A measurement that happens to be whole reads as `6 ms`, not `6.0 ms`, so
  # the table does not mix `6.0` with the counts it renders beside them.
  def plain: if . == floor then (floor | tostring) else tostring end;
  def rendered_budget:
    if .comparison == "minimum" then "at least " + (.budget | plain)
    else (.budget | plain) + " " + .unit
    end;
  def rendered_actual:
    (.actual | plain) +
    (if .unit == "count" then "" else " " + .unit end);
  "# Reproducible performance baseline\n",
  "Revision: `" + .revision + "`",
  "Workload version: `" + (.workloadVersion | tostring) + "`",
  "Overall result: **" + .overallResult + "**\n",
  "## Environment\n",
  "- OS: `" + .environment.os + "`",
  "- Architecture: `" + .environment.architecture + "`",
  "- Logical CPUs: `" + (.environment.logicalCpuCount | tostring) + "`",
  "- Rust: `" + .environment.rustc + "`",
  "- Profile: `" + .environment.profile + "`\n",
  "## Regression budget\n",
  "| Measurement | Observed | Budget | Result |",
  "| --- | ---: | ---: | --- |",
  (.budgetChecks[] |
    "| " + .name + " | " + rendered_actual + " | " + rendered_budget +
    " | " + .result + " |"),
  "\n## Slow-peer limit behavior\n",
  "- Configured outbound message limit: `" +
    (.limitBehavior.slowPeer.outboundMessageLimit | tostring) + "`",
  "- Attempts: `" + (.limitBehavior.slowPeer.attempted | tostring) + "`",
  "- Accepted: `" + (.limitBehavior.slowPeer.accepted | tostring) + "`",
  "- Overloaded: `" + (.limitBehavior.slowPeer.overloaded | tostring) + "`",
  "- Delivered: `" + (.limitBehavior.slowPeer.delivered | tostring) + "`"
' "$output_dir/results.json" >"$output_dir/results.md"

if ! jq -e '.overallResult == "success"' "$output_dir/results.json" >/dev/null; then
    echo 'Performance baseline exceeded its regression budget' >&2
    exit 1
fi

echo "Performance baseline passed for $revision"
