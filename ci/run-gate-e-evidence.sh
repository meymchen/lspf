#!/usr/bin/env bash
set -euo pipefail

# Gate E validates the artifact the release will publish. Gates A through D
# prove things about a revision; this gate reconstructs that revision with
# `git archive` and grafts the verified candidate crate over `crates/lspf`, so
# every journey below compiles the packaged bytes rather than the workspace
# sources.

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source ci/gate-evidence-helpers.sh
source ci/release-candidate-helpers.sh

revision="${1:?usage: run-gate-e-evidence.sh REVISION RUN_URL CANDIDATE_DIRECTORY OUTPUT_DIRECTORY}"
run_url="${2:?usage: run-gate-e-evidence.sh REVISION RUN_URL CANDIDATE_DIRECTORY OUTPUT_DIRECTORY}"
candidate_dir="${3:?usage: run-gate-e-evidence.sh REVISION RUN_URL CANDIDATE_DIRECTORY OUTPUT_DIRECTORY}"
output_dir="${4:?usage: run-gate-e-evidence.sh REVISION RUN_URL CANDIDATE_DIRECTORY OUTPUT_DIRECTORY}"
cargo_bin="${CARGO_BIN:-cargo}"
blocker_register="${RELEASE_BLOCKERS_FILE:-}"
repository="${GITHUB_REPOSITORY:-meymchen/lspf}"
server_url="${GITHUB_SERVER_URL:-https://github.com}"

if [[ ! $revision =~ ^[0-9a-f]{40}$ ]]; then
    printf 'Gate E evidence revision must be a full lowercase commit SHA: %s\n' \
        "$revision" >&2
    exit 1
fi
if [[ ! $run_url =~ ^https:// ]]; then
    printf 'Gate E evidence run URL must use HTTPS: %s\n' "$run_url" >&2
    exit 1
fi
if [[ -e $output_dir ]]; then
    printf 'Gate E evidence output already exists: %s\n' "$output_dir" >&2
    exit 1
fi

validate_candidate_metadata "$revision" "$candidate_dir/candidate-metadata.json"
validate_release_gate_evidence "$revision" "$candidate_dir/evidence"

read_release_crate_identity "$candidate_dir/release-metadata.json"
candidate_crate="$(cd "$candidate_dir" && pwd)/$crate_file"
if [[ -n $blocker_register ]]; then
    blocker_register="$(cd "$(dirname "$blocker_register")" && pwd)/$(basename "$blocker_register")"
fi

mkdir -p "$output_dir/runs"
output_dir="$(cd "$output_dir" && pwd)"
runs="$output_dir/runs.json"
printf '[]\n' >"$runs"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/tree" "$work/unpack" "$work/install"
tree="$work/tree"

echo "Reconstructing $revision and grafting the candidate crate into crates/lspf"
git archive --format=tar "$revision" | tar -x -C "$tree"
tar -xzf "$candidate_crate" -C "$work/unpack"
rm -rf "$tree/crates/lspf"
mv "$work/unpack/$package_root" "$tree/crates/lspf"

if ! jq -e --arg revision "$revision" '
    .git.sha1 == $revision and ((.git.dirty // false) == false)
  ' "$tree/crates/lspf/.cargo_vcs_info.json" >/dev/null 2>&1
then
    echo 'the grafted candidate crate does not identify the validated revision as clean source' >&2
    exit 1
fi

candidate_sha256="$(sha256sum "$candidate_crate" | awk '{print $1}')"
# The register defaults to the revision-locked copy inside the reconstructed
# worktree, so the gate reads the same disposition the release ships.
blocker_register="${blocker_register:-$tree/ci/release-blockers-v1.json}"
server_binary="$work/install/bin/lspf-markdown"
if [[ ${OS:-} == Windows_NT ]]; then
    server_binary="$server_binary.exe"
fi

run_journey() {
    local id=$1
    local log=$2
    local commands_file=$3

    case "$id" in
        candidate-compatibility)
            run_logged "$log" "$commands_file" \
                "$cargo_bin" test -p lspf --locked --features testing \
                --test frozen_interface -- --test-threads=1
            ;;
        candidate-overload)
            run_logged "$log" "$commands_file" \
                "$cargo_bin" test -p lspf --locked \
                --test resource_policy --test error_hook -- --test-threads=1
            ;;
        candidate-timeout)
            run_logged "$log" "$commands_file" \
                "$cargo_bin" test -p lspf --locked \
                --test inbound_completion --test public_conformance \
                -- --test-threads=1
            ;;
        candidate-disconnect)
            run_logged "$log" "$commands_file" \
                "$cargo_bin" test -p lspf --locked \
                --test client_endpoint --test session_close -- --test-threads=1
            ;;
        candidate-child-cleanup)
            run_logged "$log" "$commands_file" \
                "$cargo_bin" test -p lspf --locked \
                --test stdio_child -- --test-threads=1
            ;;
        candidate-reference-server)
            run_logged "$log" "$commands_file" \
                "$cargo_bin" test -p lspf-markdown --all-targets --locked \
                -- --test-threads=1
            ;;
        candidate-editor-journeys)
            # The editors launch an installed `lspf-markdown` selected through
            # LSPF_MARKDOWN_SERVER; install it from the grafted tree so the
            # journey drives a server linked against the candidate crate. The
            # selection runs through `env` so the recorded command names it.
            run_logged "$log" "$commands_file" \
                "$cargo_bin" install --path crates/lspf-markdown \
                --root "$work/install" --locked --force \
                && run_logged "$log" "$commands_file" \
                    env "LSPF_MARKDOWN_SERVER=$server_binary" \
                    "$cargo_bin" test -p lspf-markdown \
                    --test packaged_editor_journey -- --test-threads=1 \
                && run_logged "$log" "$commands_file" \
                    bash ci/check-editor-validation.sh "$tree"
            ;;
        release-blocker-register)
            run_logged "$log" "$commands_file" \
                bash ci/check-release-blockers.sh "$blocker_register" \
                editor-validation/journeys-v1.json
            ;;
    esac
}

record_run() {
    local id=$1
    local name=$2
    local directory="$output_dir/runs/$id"
    local log="$directory/command.log"
    local command commands_file result status started_ns finished_ns duration_ms
    local explanation failure_summary

    mkdir -p "$directory"
    commands_file="$(mktemp)"
    printf 'Revision: %s\nCandidate: %s\n' "$revision" "$package_root" >"$log"

    started_ns="$(date +%s%N)"
    set +e
    (
        cd "$tree"
        run_journey "$id" "$log" "$commands_file"
    ) 2>&1 | tee -a "$log"
    status=${PIPESTATUS[0]}
    set -e
    finished_ns="$(date +%s%N)"
    duration_ms=$(((finished_ns - started_ns) / 1000000))
    command="$(joined_logged_commands "$commands_file")"
    rm -f "$commands_file"

    if ((status == 0)); then
        result=success
        explanation=''
    else
        result=failure
        failure_summary="$(awk 'NF {last=$0} END {print last}' "$log")"
        explanation="Command exited with status $status. Last non-empty output: $failure_summary. The candidate cannot be published until this journey passes against the packaged crate."
    fi
    {
        printf 'Result: %s\nDuration milliseconds: %s\nExit status: %s\n' \
            "$result" "$duration_ms" "$status"
    } >>"$log"

    jq \
        --arg id "$id" \
        --arg name "$name" \
        --arg command "$command" \
        --arg result "$result" \
        --arg explanation "$explanation" \
        --arg log "runs/$id/command.log" \
        --argjson duration "$duration_ms" '
          . + [{
            id: $id,
            name: $name,
            command: $command,
            result: $result,
            durationMilliseconds: $duration,
            explanation: (if $explanation == "" then null else $explanation end),
            log: $log
          }]
        ' "$runs" >"$runs.next"
    mv "$runs.next" "$runs"
}

record_run candidate-compatibility 'Frozen public interface compatibility'
record_run candidate-overload 'Bounded admission overload'
record_run candidate-timeout 'Handler and outbound timeout'
record_run candidate-disconnect 'Disconnect resolves pending work'
record_run candidate-child-cleanup 'Child process cleanup'
record_run candidate-reference-server 'Reference server on the candidate'
record_run candidate-editor-journeys 'Editor journeys on the installed candidate server'
record_run release-blocker-register 'Release blocker register'

# A register the check already rejected may not be readable JSON. Report it as
# empty rather than losing the whole evidence bundle to a parse error.
register_report_file="$blocker_register"
if ! jq -e 'type == "object"' "$blocker_register" >/dev/null 2>&1; then
    register_report_file="$output_dir/unreadable-blocker-register.json"
    printf '{"schemaVersion":1,"blockers":[]}\n' >"$register_report_file"
fi

jq -n \
    --arg revision "$revision" \
    --arg run "$run_url" \
    --arg repository "$repository" \
    --arg server "$server_url" \
    --arg crate "$crate_name" \
    --arg version "$crate_version" \
    --arg crate_file "$crate_file" \
    --arg sha256 "$candidate_sha256" \
    --slurpfile runs "$runs" \
    --slurpfile register "$register_report_file" \
    --slurpfile editors "$tree/editor-validation/journeys-v1.json" '
    def source($path):
      ($server + "/" + $repository + "/blob/" + $revision + "/" + $path);
    def run($id): ($runs[0][] | select(.id == $id));
    def validation($id; $statement; $run_ids):
      [$run_ids[] as $run_id | run($run_id)] as $evidence
      | {
          id: $id,
          statement: $statement,
          result: (if all($evidence[]; .result == "success")
            then "success" else "failure" end),
          evidence: [$evidence[] | {id, command, log}]
        };
    {
      schemaVersion: 1,
      gate: "E",
      revision: $revision,
      sourceRepository: ($server + "/" + $repository),
      workflowRun: $run,
      candidate: {
        crate: $crate,
        version: $version,
        artifact: $crate_file,
        sha256: $sha256,
        graft: "crates/lspf"
      },
      sources: [
        source("ci/run-gate-e-evidence.sh"),
        source("ci/check-release-blockers.sh"),
        source("ci/release-blockers-v1.json"),
        source("editor-validation/journeys-v1.json"),
        source("crates/lspf-markdown/tests/packaged_editor_journey.rs")
      ],
      runs: $runs[0],
      validations: [
        validation("candidate-artifact";
          "The reference server and the editor journeys build and run against the packaged candidate crate, not the workspace sources.";
          ["candidate-reference-server", "candidate-editor-journeys"]),
        validation("compatibility";
          "The packaged crate still presents the frozen 1.0 public interface item by item.";
          ["candidate-compatibility"]),
        validation("overload";
          "Inbound, outbound, and document admission reject work beyond the configured bounds and report each rejection once.";
          ["candidate-overload"]),
        validation("timeout";
          "Handler and outbound request deadlines are finite, enforced, and complete their pending work exactly once.";
          ["candidate-timeout"]),
        validation("disconnect";
          "Disconnecting a peer resolves pending requests and closes the session without leaking connection tasks.";
          ["candidate-disconnect"]),
        validation("child-cleanup";
          "A supervised stdio child is terminated, killed when it will not exit, and reaped to a terminal status.";
          ["candidate-child-cleanup"]),
        validation("no-undisposed-blocker";
          "No framework-owned P0 or P1 blocker is left without a disposition in the release blocker register.";
          ["release-blocker-register"])
      ],
      blockerRegister: {
        source: source("ci/release-blockers-v1.json"),
        result: run("release-blocker-register").result,
        recorded: ($register[0].blockers | length),
        acceptedFrameworkP0P1: [
          $register[0].blockers[]
          | select(.owner == "framework"
            and (.severity | IN("P0", "P1"))
            and .disposition == "accepted")
        ]
      },
      editorObservations: {
        classification: "human",
        status: $editors[0].humanUxObservations.status,
        recorded: ($editors[0].humanUxObservations.records | length),
        editors: [$editors[0].editors[].id]
      },
      humanJudgments: [{
        classification: "human",
        status: "pending",
        statement: "Whether an accepted framework-owned P0 or P1 blocker is tolerable in a 1.0 release is a maintainer judgment; this gate only proves that none is left undisposed."
      }, {
        classification: "human",
        status: $editors[0].humanUxObservations.status,
        statement: ("Driving the " + ([$editors[0].editors[].id] | join(", "))
          + " user interfaces against this candidate is a human observation this gate does not perform; the automated journey it runs proves protocol behavior only.")
      }]
    }
    | .failedJourneys = (
        [.runs[] | select(.result != "success") | {id, name, result, explanation, log}]
        + [.validations[] | select(.result != "success")
          | {id, name: .statement, result, explanation: null, log: null}]
        | unique_by(.id)
      )
    | .overallResult =
        (if (.failedJourneys | length) == 0 then "success" else "failure" end)
  ' >"$output_dir/evidence.json"

run_number="${run_url##*/}"
jq -r --arg run_number "$run_number" '
  "# Gate E candidate validation evidence\n",
  "Revision: [" + .revision + "](" + .sourceRepository + "/commit/" + .revision + ")",
  (if .overallResult == "success" then "Passing run: " else "Workflow run: " end)
    + "[CI run " + $run_number + "](" + .workflowRun + ")",
  "Candidate: `" + .candidate.artifact + "` (`sha256:" + .candidate.sha256 + "`)",
  "Overall result: **" + .overallResult + "**\n",
  "## Journeys against the candidate artifact\n",
  (.runs[] |
    "- " + .name + ": `" + .result + "` in `"
      + (.durationMilliseconds | tostring) + " ms`. Command: `" + .command
      + "`. [Log](" + .log + ")"),
  "\n## Validated properties\n",
  (.validations[] | "- " + .statement + " Result: `" + .result + "`."),
  "\n## Release blocker register\n",
  "- Recorded blockers: `" + (.blockerRegister.recorded | tostring)
    + "`. Register check: `" + .blockerRegister.result + "`.",
  "- Framework-owned P0 or P1 accepted by maintainers: `"
    + (.blockerRegister.acceptedFrameworkP0P1 | length | tostring) + "`.",
  (.blockerRegister.acceptedFrameworkP0P1[] |
    "  - " + .severity + " " + .id + " (" + .issue + "): " + .justification),
  "\n## Editor matrix observations\n",
  "- Editors: `" + (.editorObservations.editors | join("`, `")) + "`.",
  "- Human observation status: `" + .editorObservations.status
    + "` with `" + (.editorObservations.recorded | tostring) + "` recorded.",
  (if (.failedJourneys | length) > 0 then
    "\n## Failing journeys\n",
    (.failedJourneys[] |
      "- " + .name + ": `" + .result + "`." + (if .explanation then " " + .explanation else "" end))
  else empty end),
  "\n## Human judgments\n",
  "These items are deliberately not presented as automated proof.\n",
  (.humanJudgments[] | "- " + .statement + " Status: `" + .status + "`."),
  "\n## Revision-locked sources\n",
  (.sources[] | "- [Source](" + . + ")")
' "$output_dir/evidence.json" >"$output_dir/evidence.md"

if ! jq -e '.overallResult == "success"' "$output_dir/evidence.json" >/dev/null; then
    echo 'Gate E evidence contains failing journeys' >&2
    exit 1
fi

echo "Gate E candidate validation evidence prepared for $revision"
