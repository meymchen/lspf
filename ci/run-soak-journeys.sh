#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

revision="${1:?usage: run-soak-journeys.sh <40-character revision> <output-directory>}"
output_dir="${2:?usage: run-soak-journeys.sh <40-character revision> <output-directory>}"

if [[ ! $revision =~ ^[0-9a-f]{40}$ ]]; then
    echo 'Soak revision must be a 40-character lowercase hexadecimal SHA' >&2
    exit 2
fi

workloads="$(realpath -m "${SOAK_WORKLOADS:-soak/workloads-v2.json}")"
thresholds="$(realpath -m "${SOAK_THRESHOLDS:-soak/thresholds-v2.json}")"
output_dir="$(realpath -m "$output_dir")"
raw_results="$output_dir/raw-results.json"
time_series_jsonl="$output_dir/time-series.jsonl"
cargo_bin="${CARGO_BIN:-cargo}"

for input in "$workloads" "$thresholds"; do
    if [[ ! -f $input ]]; then
        echo "Soak input does not exist: $input" >&2
        exit 2
    fi
done

workload_version="$(jq -er '.workloadVersion | numbers' "$workloads")"
threshold_version="$(jq -er '.workloadVersion | numbers' "$thresholds")"
maximum_run_seconds="$(jq -er '.maximumRunSeconds | numbers' "$thresholds")"
if [[ $workload_version != "$threshold_version" ]]; then
    echo 'Soak workload and threshold versions do not match' >&2
    exit 2
fi

mkdir -p "$output_dir"
cp "$workloads" "$output_dir/workloads.json"
cp "$thresholds" "$output_dir/thresholds.json"
: >"$time_series_jsonl"

set +e
timeout --signal=TERM --kill-after=15s "${maximum_run_seconds}s" \
    "$cargo_bin" bench -p lspf --bench soak_journeys --features testing -- \
    --workloads "$workloads" \
    --output "$raw_results" \
    --timeseries "$time_series_jsonl" \
    --revision "$revision" 2>&1 | tee "$output_dir/command.log"
command_status=${PIPESTATUS[0]}
set -e

case $command_status in
    0) command_outcome=success ;;
    124|137) command_outcome=timeout ;;
    *) command_outcome=failure ;;
esac

if [[ ! -f $raw_results ]]; then
    jq -n \
        --arg revision "$revision" \
        --argjson workload_version "$workload_version" '
      {
        schemaVersion: 1,
        workloadVersion: $workload_version,
        revision: $revision,
        durationSeconds: 0,
        traffic: {operations: 0, bytes: 0},
        peakRssMiB: 0,
        unexplainedGrowthMiB: 0,
        scenarios: []
      }
    ' >"$raw_results"
fi

jq -s '.' "$time_series_jsonl" >"$output_dir/time-series.json"

jq \
    --arg revision "$revision" \
    --arg command_outcome "$command_outcome" \
    --argjson workload_version "$workload_version" \
    --slurpfile workload "$workloads" \
    --slurpfile thresholds "$thresholds" \
    --slurpfile time_series "$output_dir/time-series.json" '
  def check($id; $name; $actual; $limit; $comparison; $unit; $success):
    {
      id: $id,
      name: $name,
      actual: $actual,
      limit: $limit,
      comparison: $comparison,
      unit: $unit,
      result: (if $success then "success" else "failure" end)
    };
  def terminal_empty:
    (.terminalResources | type == "object")
    and all(.terminalResources[]; . == 0);
  $workload[0].scenarios as $required
  | . as $raw
  | $thresholds[0] as $limits
  | .revision = $revision
  | .workloadVersion = $workload_version
  | .commandOutcome = $command_outcome
  | .timeSeries = $time_series[0]
  | (.timeSeries[-1].scenario // "process") as $active_scenario
  | if $command_outcome != "success"
      and (.timeSeries | length) > 0
      and ([.scenarios[].name] | index($active_scenario)) == null
    then .scenarios += [{
      name: .timeSeries[-1].scenario,
      result: "failure",
      terminalOutcome: $command_outcome,
      durationMilliseconds: 0,
      operations: 0,
      bytes: 0,
      terminalResources: .timeSeries[-1].resources
    }]
    else .
    end
  | .scenarios = [
      .scenarios[]
      | if (.result == "success" and terminal_empty) then .
        else .result = "failure"
        end
    ]
  | .thresholdChecks = [
      check("peak-rss"; "Peak RSS"; .peakRssMiB;
        $limits.maximumPeakRssMiB; "maximum"; "MiB";
        .peakRssMiB <= $limits.maximumPeakRssMiB),
      check("unexplained-memory-growth"; "Unexplained memory growth";
        .unexplainedGrowthMiB; $limits.maximumUnexplainedGrowthMiB;
        "maximum"; "MiB";
        .unexplainedGrowthMiB <= $limits.maximumUnexplainedGrowthMiB),
      check("samples-per-scenario"; "Samples per scenario";
        ($required
          | map(. as $name | [$time_series[0][] | select(.scenario == $name)] | length)
          | min // 0);
        $limits.minimumSamplesPerScenario; "minimum"; "count";
        (all($required[];
          . as $name
          | ([$time_series[0][] | select(.scenario == $name)] | length)
              >= $limits.minimumSamplesPerScenario)))
    ]
  | .failedChecks = (
      [.thresholdChecks[] | select(.result != "success")]
      + [.scenarios[] | select(.result != "success")
          | {id:("scenario-" + .name), name:(.name + " scenario"), result:"failure"}]
      + (if $command_outcome == "success" then [] else
          [{id:"command-outcome", name:"Soak process", actual:$command_outcome,
            result:"failure"}]
        end)
      + (if ([.scenarios[].name] | sort) == ($required | sort)
        then [] else
          [{id:"scenario-coverage", name:"Required scenario coverage", result:"failure"}]
        end)
    )
  | .overallResult =
      (if (.failedChecks | length) == 0 then "success" else "failure" end)
' "$raw_results" >"$output_dir/results.json"

jq -r '
  def threshold_limit:
    (if .comparison == "minimum" then "at least " else "at most " end)
    + (.limit | tostring) + (if .unit == "count" then "" else " " + .unit end);
  def observed:
    (.actual | tostring) + (if .unit == "count" then "" else " " + .unit end);
  "# Bounded-memory soak journeys\n",
  "Revision: `" + .revision + "`",
  "Workload version: `" + (.workloadVersion | tostring) + "`",
  "Measured duration: `" + (.durationSeconds | tostring) + " seconds`",
  "Command outcome: `" + .commandOutcome + "`",
  "Overall result: **" + .overallResult + "**\n",
  "## Terminal outcomes\n",
  "| Scenario | Result | Terminal outcome |",
  "| --- | --- | --- |",
  (.scenarios[] |
    "| `" + .name + "` | " + .result + " | `" + .terminalOutcome + "` |"),
  "\n## Thresholds\n",
  "| Measurement | Observed | Threshold | Result |",
  "| --- | ---: | ---: | --- |",
  (.thresholdChecks[] |
    "| " + .name + " | " + observed + " | " + threshold_limit + " | " + .result + " |"),
  "\n## Traffic\n",
  "- Operations: `" + (.traffic.operations | tostring) + "`",
  "- Payload bytes: `" + (.traffic.bytes | tostring) + "`",
  "- Time-series samples: `" + (.timeSeries | length | tostring) + "`"
' "$output_dir/results.json" >"$output_dir/results.md"

if ! jq -e '.overallResult == "success"' "$output_dir/results.json" >/dev/null; then
    echo 'Bounded-memory soak journeys failed' >&2
    exit 1
fi

echo "Bounded-memory soak journeys passed for $revision"
