#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

revision="${1:?usage: run-gate-d-evidence.sh REVISION RUN_URL OUTPUT_DIRECTORY}"
run_url="${2:?usage: run-gate-d-evidence.sh REVISION RUN_URL OUTPUT_DIRECTORY}"
output_dir="${3:?usage: run-gate-d-evidence.sh REVISION RUN_URL OUTPUT_DIRECTORY}"
repository="${GITHUB_REPOSITORY:-meymchen/lspf}"
server_url="${GITHUB_SERVER_URL:-https://github.com}"
component_runner="${GATE_D_COMPONENT_RUNNER:-}"

if [[ ! $revision =~ ^[0-9a-f]{40}$ ]]; then
    printf 'Gate D evidence revision must be a full lowercase commit SHA: %s\n' \
        "$revision" >&2
    exit 1
fi
if [[ ! $run_url =~ ^https:// ]]; then
    printf 'Gate D evidence run URL must use HTTPS: %s\n' "$run_url" >&2
    exit 1
fi
if [[ -e $output_dir ]]; then
    printf 'Gate D evidence output already exists: %s\n' "$output_dir" >&2
    exit 1
fi

mkdir -p "$output_dir/components"
output_dir="$(cd "$output_dir" && pwd)"
components="$output_dir/components.json"
printf '[]\n' >"$components"

run_logged() {
    local log=$1
    local commands_file=$2
    shift 2
    local rendered

    printf -v rendered '%q ' "$@"
    rendered=${rendered% }
    printf '%s\n' "$rendered" >>"$commands_file"
    printf '$ %s\n' "$rendered" >>"$log"
    "$@"
}

run_default_component() {
    local id=$1
    local directory=$2
    local log=$3
    local commands_file=$4

    case "$id" in
        fuzz)
            FUZZ_ARTIFACT_ROOT="$directory/reproducers" \
                run_logged "$log" "$commands_file" bash ci/run-fuzz.sh --all
            ;;
        model)
            run_logged "$log" "$commands_file" \
                cargo test -p lspf --test concurrency_model \
                --no-default-features -- --test-threads=1
            ;;
        performance)
            run_logged "$log" "$commands_file" \
                bash ci/run-performance-baseline.sh "$revision" "$directory/data"
            ;;
        soak)
            run_logged "$log" "$commands_file" \
                bash ci/run-soak-journeys.sh "$revision" "$directory/data"
            ;;
        reference-server)
            run_logged "$log" "$commands_file" \
                cargo test -p lspf-markdown --all-targets -- --test-threads=1
            ;;
        editor)
            run_logged "$log" "$commands_file" \
                cargo test -p lspf-markdown --test packaged_editor_journey -- \
                --test-threads=1 && \
                run_logged "$log" "$commands_file" \
                    bash ci/check-editor-validation.sh
            ;;
    esac
}

record_component() {
    local id=$1
    local name=$2
    shift 2
    local directory="$output_dir/components/$id"
    local log="$directory/command.log"
    local command commands_file result status started_ns finished_ns duration_ms explanation
    local failure_analysis failure_summary configuration_json configuration_urls_json

    configuration_json="$(printf '%s\n' "$@" | jq -Rsc 'split("\n") | map(select(length > 0))')"
    configuration_urls_json="$(jq \
        --arg revision "$revision" \
        --arg repository "$repository" \
        --arg server "$server_url" '
          [.[] | ($server + "/" + $repository + "/blob/" + $revision + "/" + .)]
        ' <<<"$configuration_json")"
    mkdir -p "$directory"
    commands_file="$(mktemp)"
    {
        printf 'Revision: %s\nConfiguration:\n' "$revision"
        jq -r '.[] | "- " + .' <<<"$configuration_urls_json"
    } >"$log"

    started_ns="$(date +%s%N)"
    set +e
    if [[ -n $component_runner ]]; then
        run_logged "$log" "$commands_file" \
            "$component_runner" "$id" "$revision" "$directory" \
            2>&1 | tee -a "$log"
    else
        run_default_component "$id" "$directory" "$log" "$commands_file" \
            2>&1 | tee -a "$log"
    fi
    status=${PIPESTATUS[0]}
    set -e
    finished_ns="$(date +%s%N)"
    duration_ms=$(((finished_ns - started_ns) / 1000000))
    command="$(awk 'NR == 1 {combined=$0; next} {combined=combined " && " $0} END {print combined}' "$commands_file")"
    rm -f "$commands_file"

    if ((status == 0)); then
        result=success
        explanation=''
        failure_analysis=''
    else
        result=failure
        failure_analysis=requires-analysis
        failure_summary="$(awk 'NF {last=$0} END {print last}' "$log")"
        explanation="Command exited with status $status. Last non-empty output: $failure_summary. The cause requires analysis before Gate D can pass; inspect the full component log and retained outputs."
    fi
    {
        printf 'Result: %s\nDuration milliseconds: %s\nExit status: %s\n' \
            "$result" "$duration_ms" "$status"
        if [[ -n $failure_analysis ]]; then
            printf 'Failure analysis: %s\n' "$failure_analysis"
        fi
    } >>"$log"

    jq \
        --arg id "$id" \
        --arg name "$name" \
        --arg revision "$revision" \
        --arg command "$command" \
        --arg result "$result" \
        --arg explanation "$explanation" \
        --arg failure_analysis "$failure_analysis" \
        --arg log "components/$id/command.log" \
        --argjson duration "$duration_ms" \
        --argjson configuration "$configuration_urls_json" '
          . + [{
            id: $id,
            name: $name,
            revision: $revision,
            configuration: $configuration,
            durationMilliseconds: $duration,
            command: $command,
            result: $result,
            explanation: (if $explanation == "" then null else $explanation end),
            failureAnalysis: (if $failure_analysis == "" then null else $failure_analysis end),
            log: $log
          }]
        ' "$components" >"$components.next"
    mv "$components.next" "$components"
    jq --arg id "$id" '.[] | select(.id == $id)' \
        "$components" >"$directory/metadata.json"
}

record_component fuzz 'Bounded fuzz targets' \
    fuzz/README.md ci/run-fuzz.sh fuzz/Cargo.toml
record_component model 'Model interleavings' \
    crates/lspf/tests/concurrency_model.rs \
    crates/lspf/tests/concurrency_model_support/mod.rs
record_component performance 'Performance baseline' \
    docs/performance-baselines.md performance/workloads-v1.json \
    performance/regression-budget-v1.json
record_component soak 'Bounded-memory soak journeys' \
    docs/soak-journeys.md soak/workloads-v2.json soak/thresholds-v2.json
record_component reference-server 'Reference server' \
    crates/lspf-markdown/Cargo.toml crates/lspf-markdown/src/main.rs \
    crates/lspf-markdown/tests/packaged_editor_journey.rs
record_component editor 'Editor journeys' \
    editor-validation/README.md editor-validation/journeys-v1.json \
    ci/check-editor-validation.sh

performance_results="$output_dir/components/performance/results.json"
if [[ ! -f $performance_results ]]; then
    performance_results="$output_dir/components/performance/data/results.json"
fi
if [[ ! -f $performance_results ]]; then
    printf '{}\n' >"$output_dir/missing-performance-results.json"
    performance_results="$output_dir/missing-performance-results.json"
fi

jq -n \
    --arg revision "$revision" \
    --arg run "$run_url" \
    --arg repository "$repository" \
    --arg server "$server_url" \
    --slurpfile components "$components" \
    --slurpfile performance "$performance_results" '
    def component($id): ($components[0][] | select(.id == $id));
    {
      schemaVersion: 1,
      gate: "D",
      revision: $revision,
      sourceRepository: ($server + "/" + $repository),
      workflowRun: $run,
      components: $components[0],
      performanceClaims: ($performance[0] | {
        revision,
        latencyMs,
        throughputOperationsPerSecond,
        peakRssMiB,
        limitBehavior,
        budgetChecks,
        overallResult
      }),
      publicInterfaceEvidence: {
        crate: "lspf-markdown",
        result: component("reference-server").result,
        reason: "The reference server is a separate workspace crate and its tests consume lspf through the public dependency boundary.",
        sources: component("reference-server").configuration
      },
      editorEvidence: {
        classification: "automated",
        result: component("editor").result,
        represented: ["open", "edit", "diagnostics", "hover", "definition", "restart", "shutdown"],
        configuration: component("editor").configuration
      },
      humanJudgments: [{
        classification: "human",
        status: "not-evaluated-by-gate",
        statement: "Editor UI quality and the final 1.0 release decision remain human judgments; this artifact records only reproducible machine evidence."
      }]
    }
    | .failedComponents = [
        .components[] | select(.result != "success")
        | {id, name, result, failureAnalysis, explanation, log}
      ]
    | if (component("performance").result == "success"
        and (.performanceClaims.revision != $revision
          or .performanceClaims.overallResult != "success")) then
        .failedComponents += [{
          id: "performance-data",
          name: "Performance result integrity",
          result: "failure",
          explanation: "The benchmark command succeeded without revision-matching, passing result data; no performance claim can be accepted.",
          log: component("performance").log
        }]
      else . end
    | .overallResult =
        (if (.failedComponents | length) == 0 then "success" else "failure" end)
  ' >"$output_dir/evidence.json"

run_number="${run_url##*/}"
jq -r --arg run_number "$run_number" '
  "# Gate D verification evidence\n",
  "Revision: [" + .revision + "](" + .sourceRepository + "/commit/" + .revision + ")",
  (if .overallResult == "success" then "Passing run: " else "Workflow run: " end)
    + "[CI run " + $run_number + "](" + .workflowRun + ")",
  "Overall result: **" + .overallResult + "**\n",
  "## Verification components\n",
  (.components[] |
    "- " + .name + ": `" + .result + "` in `"
      + (.durationMilliseconds | tostring) + " ms`. Command: `" + .command
      + "`. [Log](" + .log + ") Configuration: "
      + ([.configuration[] | "[revision-locked source](" + . + ")"] | join(", "))),
  "\n## Performance claims\n",
  "- Request p99 latency: `" + (.performanceClaims.latencyMs.requestP99 | tostring) + " ms`",
  "- Throughput: `" + (.performanceClaims.throughputOperationsPerSecond | tostring) + " operations/s`",
  "- Peak RSS: `" + (.performanceClaims.peakRssMiB | tostring) + " MiB`",
  "- Slow-peer overloads: `" + (.performanceClaims.limitBehavior.slowPeer.overloaded | tostring) + "`",
  "\n## Public-interface evidence\n",
  "- `" + .publicInterfaceEvidence.crate + "`: `" + .publicInterfaceEvidence.result
    + "`. " + .publicInterfaceEvidence.reason,
  (if (.failedComponents | length) > 0 then
    "\n## Failing components\n",
    (.failedComponents[] |
      "- " + .name + ": `" + .result + "`. " + .explanation
        + " [Log](" + .log + ")")
  else empty end),
  "\n## Human judgments\n",
  (.humanJudgments[] | "- " + .statement + " Status: `" + .status + "`.")
' "$output_dir/evidence.json" >"$output_dir/evidence.md"

if ! jq -e '.overallResult == "success"' "$output_dir/evidence.json" >/dev/null; then
    echo 'Gate D evidence contains failing or missing components' >&2
    exit 1
fi

echo "Gate D verification evidence prepared for $revision"
