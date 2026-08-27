#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

revision="${1:?usage: run-gate-c-evidence.sh REVISION RUN_URL OUTPUT_DIRECTORY}"
run_url="${2:?usage: run-gate-c-evidence.sh REVISION RUN_URL OUTPUT_DIRECTORY}"
output_dir="${3:?usage: run-gate-c-evidence.sh REVISION RUN_URL OUTPUT_DIRECTORY}"
cargo_bin="${CARGO_BIN:-cargo}"
repository="${GITHUB_REPOSITORY:-meymchen/lspf}"
server_url="${GITHUB_SERVER_URL:-https://github.com}"

if [[ ! $revision =~ ^[0-9a-f]{40}$ ]]; then
    printf 'Gate C evidence revision must be a full lowercase commit SHA: %s\n' \
        "$revision" >&2
    exit 1
fi
if [[ ! $run_url =~ ^https:// ]]; then
    printf 'Gate C evidence run URL must use HTTPS: %s\n' "$run_url" >&2
    exit 1
fi
if [[ -e $output_dir ]]; then
    printf 'Gate C evidence output already exists: %s\n' "$output_dir" >&2
    exit 1
fi

mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"
results="$output_dir/results.json"
commands_log="$output_dir/commands.log"
printf '[]\n' >"$results"
: >"$commands_log"

run_cargo() {
    local id="$1"
    local name="$2"
    shift 2
    local command result temporary

    command="cargo $*"
    printf '$ %s\n' "$command" | tee -a "$commands_log"
    temporary="$(mktemp)"
    if "$cargo_bin" "$@" >"$temporary" 2>&1; then
        result=success
    else
        result=failure
    fi
    tee -a "$commands_log" <"$temporary"
    rm -f "$temporary"

    jq \
        --arg id "$id" \
        --arg name "$name" \
        --arg command "$command" \
        --arg result "$result" \
        '. + [{id: $id, name: $name, command: $command, result: $result}]' \
        "$results" >"$results.next"
    mv "$results.next" "$results"
}

run_cargo public-conformance "Downstream-only public conformance journeys" \
    test -p lspf --test public_conformance -- --test-threads=1
run_cargo client-adoption-doctests "Public Client adoption walkthroughs" \
    test -p lspf --doc markdown::ClientAdoptionGuide -- --test-threads=1

jq -n \
    --arg revision "$revision" \
    --arg run "$run_url" \
    --arg repository "$repository" \
    --arg server "$server_url" \
    --slurpfile runs "$results" '
    def source($path):
      ($server + "/" + $repository + "/blob/" + $revision + "/" + $path);
    def run($id): ($runs[0][] | select(.id == $id));
    def journey($id; $name; $represented):
      run("public-conformance") as $evidence
      | {
          id: $id,
          name: $name,
          result: $evidence.result,
          command: $evidence.command,
          represented: $represented
        };
    def invariant($id; $statement; $run_ids):
      [$run_ids[] as $run_id | run($run_id)] as $evidence
      | {
          id: $id,
          statement: $statement,
          result: (if all($evidence[]; .result == "success") then "success" else "failure" end),
          evidence: [$evidence[].command]
        };
    {
      schemaVersion: 1,
      gate: "C",
      revision: $revision,
      sourceRepository: ($server + "/" + $repository),
      workflowRun: $run,
      sources: [
        source("crates/lspf/tests/public_conformance.rs"),
        source("docs/guides/client-adoption.md"),
        source("docs/adr/0029-client-lifecycle-control.md"),
        source("docs/adr/0030-client-context-stays-protocol-only.md"),
        source("docs/adr/0031-stdio-child-owns-supervision.md")
      ],
      runs: $runs[0],
      journeys: [
        journey("custom-transport"; "Custom Transport"; [
          "initialize",
          "typed-calls",
          "reverse-calls",
          "cancellation",
          "timeout",
          "transport-failure",
          "shutdown"
        ]),
        journey("stdio-child"; "Real stdio child"; [
          "initialize",
          "typed-calls",
          "reverse-calls",
          "stderr-drain",
          "shutdown",
          "abnormal-exit"
        ])
      ],
      invariants: [
        invariant("public-only"; "The journeys and walkthroughs compile and run as external consumers of documented public interfaces."; ["public-conformance", "client-adoption-doctests"]),
        invariant("pending-work-resolved"; "Cancellation, timeout, Transport failure, and abnormal exit resolve pending futures and connection tasks."; ["public-conformance"]),
        invariant("child-reaped"; "Graceful shutdown and abnormal exit return terminal child status after stderr is drained and the process is reaped."; ["public-conformance"])
      ]
    }
    | .failedChecks = (
        ([.runs[] | select(.result != "success") | {id, name, result}]
        + [.journeys[] | select(.result != "success") | {id, name, result}]
        + [.invariants[] | select(.result != "success") | {id, name: .statement, result}])
        | unique_by(.id)
      )
    | .overallResult = (if (.failedChecks | length) == 0 then "success" else "failure" end)
  ' >"$output_dir/evidence.json"

run_number="${run_url##*/}"
jq -r --arg run_number "$run_number" '
  "# Gate C endpoint evidence\n",
  "Revision: [" + .revision + "](" + .sourceRepository + "/commit/" + .revision + ")",
  (if .overallResult == "success" then "Passing run: " else "Workflow run: " end)
    + "[CI run " + $run_number + "](" + .workflowRun + ")",
  "Overall result: **" + .overallResult + "**\n",
  "## External-consumer journeys\n",
  (.journeys[] |
    "- " + .name + ": `" + .result + "` — " + (.represented | join(", "))
    + ". Command: `" + .command + "`."),
  "\n## Cleanup invariants\n",
  (.invariants[] | "- " + .statement + " Result: `" + .result + "`."),
  (if (.failedChecks | length) > 0 then
    "\n## Failing checks\n",
    (.failedChecks[] | "- " + .name + ": `" + .result + "`")
  else empty end),
  "\n## Revision-locked sources\n",
  (.sources[] | "- [Source](" + . + ")")
' "$output_dir/evidence.json" >"$output_dir/evidence.md"

if ! jq -e '.overallResult == "success"' "$output_dir/evidence.json" >/dev/null; then
    echo 'Gate C evidence contains failing or missing checks' >&2
    exit 1
fi

echo "Gate C endpoint evidence prepared for $revision"
