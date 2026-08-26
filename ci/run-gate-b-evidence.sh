#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

revision="${1:?usage: run-gate-b-evidence.sh REVISION RUN_URL OUTPUT_DIRECTORY}"
run_url="${2:?usage: run-gate-b-evidence.sh REVISION RUN_URL OUTPUT_DIRECTORY}"
output_dir="${3:?usage: run-gate-b-evidence.sh REVISION RUN_URL OUTPUT_DIRECTORY}"
cargo_bin="${CARGO_BIN:-cargo}"
repository="${GITHUB_REPOSITORY:-meymchen/lspf}"
server_url="${GITHUB_SERVER_URL:-https://github.com}"

if [[ ! $revision =~ ^[0-9a-f]{40}$ ]]; then
    printf 'Gate B evidence revision must be a full lowercase commit SHA: %s\n' \
        "$revision" >&2
    exit 1
fi
if [[ ! $run_url =~ ^https:// ]]; then
    printf 'Gate B evidence run URL must use HTTPS: %s\n' "$run_url" >&2
    exit 1
fi
if [[ -e $output_dir ]]; then
    printf 'Gate B evidence output already exists: %s\n' "$output_dir" >&2
    exit 1
fi

mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"
observations="$output_dir/observations.json"
results="$output_dir/results.json"
commands_log="$output_dir/commands.log"
printf '[]\n' >"$results"
: >"$commands_log"

run_test() {
    local binary="$1"
    local test_name="$2"
    local command result temporary

    command="cargo test -p lspf --test $binary $test_name -- --exact --test-threads=1"
    printf '$ %s\n' "$command" | tee -a "$commands_log"
    temporary="$(mktemp)"
    if LSPF_GATE_B_OBSERVATIONS="$observations" \
        "$cargo_bin" test -p lspf --test "$binary" "$test_name" -- \
            --exact --test-threads=1 >"$temporary" 2>&1
    then
        result=success
    else
        result=failure
    fi
    tee -a "$commands_log" <"$temporary"
    rm -f "$temporary"

    jq \
        --arg id "$test_name" \
        --arg command "$command" \
        --arg result "$result" \
        '. + [{id: $id, command: $command, result: $result}]' \
        "$results" >"$results.next"
    mv "$results.next" "$results"
}

run_test tracing_schema \
    fixed_budget_floods_and_a_slow_reader_never_exceed_connection_limits
run_test tracing_schema handler_timeout_completes_the_request_exactly_once
run_test tracing_schema eof_clears_every_connection_resource_exactly_once
run_test tracing_schema writer_failure_clears_every_connection_resource_exactly_once
run_test error_hook outbound_overload_reports_the_rejected_response_once
run_test inbound_completion \
    cancel_versus_success_race_selects_one_response_and_releases_the_id
run_test tracing_schema \
    shutdown_then_exit_clears_every_connection_resource_exactly_once

if [[ ! -f $observations ]] || ! jq -e '
    .resources | type == "array" and length > 0
    and all(.[]; .observedPeak <= .limit)
  ' "$observations" >/dev/null 2>&1
then
    printf '%s\n' '{"resources":[]}' >"$observations"
fi

jq -n \
    --arg revision "$revision" \
    --arg run "$run_url" \
    --arg repository "$repository" \
    --arg server "$server_url" \
    --slurpfile runs "$results" \
    --slurpfile observations "$observations" '
    def source($path):
      ($server + "/" + $repository + "/blob/" + $revision + "/" + $path);
    def run($id): ($runs[0][] | select(.id == $id));
    def scenario($id; $name; $tests):
      [$tests[] as $test | run($test)] as $evidence
      | {
          id: $id,
          name: $name,
          result: (if all($evidence[]; .result == "success") then "success" else "failure" end),
          command: ([$evidence[].command] | join(" && "))
        };
    def invariant($id; $statement; $tests):
      [$tests[] as $test | run($test)] as $evidence
      | {
          id: $id,
          statement: $statement,
          result: (if all($evidence[]; .result == "success") then "success" else "failure" end),
          evidence: [$evidence[] | .command]
        };
    {
      schemaVersion: 1,
      gate: "B",
      revision: $revision,
      sourceRepository: ($server + "/" + $repository),
      workflowRun: $run,
      sources: [
        source("crates/lspf/tests/tracing_schema.rs"),
        source("crates/lspf/tests/inbound_completion.rs"),
        source("crates/lspf/tests/error_hook.rs"),
        source("docs/adr/0025-one-connection-resource-policy.md"),
        source("docs/adr/0026-bounded-outbound-admission.md")
      ],
      resources: $observations[0].resources,
      scenarios: [
        scenario("flood"; "Fixed-budget request and Document flood"; ["fixed_budget_floods_and_a_slow_reader_never_exceed_connection_limits"]),
        scenario("slow-peer"; "Byte-bounded queue under a paused writer"; ["fixed_budget_floods_and_a_slow_reader_never_exceed_connection_limits"]),
        scenario("stalled-handler"; "Stalled handler deadline"; ["handler_timeout_completes_the_request_exactly_once"]),
        scenario("disconnect"; "EOF and writer-failure cleanup"; ["eof_clears_every_connection_resource_exactly_once", "writer_failure_clears_every_connection_resource_exactly_once"]),
        scenario("timeout"; "Deterministic timeout completion"; ["handler_timeout_completes_the_request_exactly_once"]),
        scenario("cancellation"; "Cancellation versus success race"; ["cancel_versus_success_race_selects_one_response_and_releases_the_id"]),
        scenario("shutdown"; "Shutdown, cleanup, and exit"; ["shutdown_then_exit_clears_every_connection_resource_exactly_once"])
      ],
      additionalRuns: [run("outbound_overload_reports_the_rejected_response_once")],
      invariants: [
        invariant("required-messages"; "Required responses either arrive or select the documented WriterFailed close outcome; none is silently lost."; ["fixed_budget_floods_and_a_slow_reader_never_exceed_connection_limits", "outbound_overload_reports_the_rejected_response_once"]),
        invariant("exactly-once"; "Every admitted request records one completion and cancellation races emit no duplicate response."; ["cancel_versus_success_race_selects_one_response_and_releases_the_id", "handler_timeout_completes_the_request_exactly_once"]),
        invariant("shutdown-terminates"; "Shutdown followed by exit completes cleanup and returns without hanging."; ["shutdown_then_exit_clears_every_connection_resource_exactly_once"])
      ]
    }
    | .failedChecks = (
        ([.scenarios[] | select(.result != "success") | {id, name, result}]
        + [.additionalRuns[] | select(.result != "success") | {id, name: .id, result}]
        + [.invariants[] | select(.result != "success") | {id, name: .statement, result}])
        | unique_by(.id)
      )
    | if (.resources | length) == 0 then
        .failedChecks += [{id: "resource-observations", name: "Resource limits and observed peaks", result: "missing"}]
      else . end
    | .overallResult = (if (.failedChecks | length) == 0 then "success" else "failure" end)
  ' >"$output_dir/evidence.json"

run_number="${run_url##*/}"
jq -r --arg run_number "$run_number" '
  "# Gate B bounded-resource evidence\n",
  "Revision: [" + .revision + "](" + .sourceRepository + "/commit/" + .revision + ")",
  (if .overallResult == "success" then "Passing run: " else "Workflow run: " end)
    + "[CI run " + $run_number + "](" + .workflowRun + ")",
  "Overall result: **" + .overallResult + "**\n",
  "## Limits and observed peaks\n",
  "| Resource | Limit | Observed peak |",
  "| --- | ---: | ---: |",
  (.resources[] | "| `" + .name + "` | " + (.limit | tostring) + " | " + (.observedPeak | tostring) + " |"),
  "\n## Scenario runs\n",
  (.scenarios[] | "- " + .name + ": `" + .result + "` — `" + .command + "`"),
  "\n## Safety invariants\n",
  (.invariants[] | "- " + .statement + " Result: `" + .result + "`."),
  (if (.failedChecks | length) > 0 then
    "\n## Failing checks\n",
    (.failedChecks[] | "- " + .name + ": `" + .result + "`")
  else empty end),
  "\n## Revision-locked sources\n",
  (.sources[] | "- [Source](" + . + ")")
' "$output_dir/evidence.json" >"$output_dir/evidence.md"

if ! jq -e '.overallResult == "success"' "$output_dir/evidence.json" >/dev/null; then
    echo 'Gate B evidence contains failing or missing checks' >&2
    exit 1
fi

echo "Gate B bounded-resource evidence prepared for $revision"
