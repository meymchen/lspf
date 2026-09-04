#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../.."
source ci/tests/candidate-fixture.sh

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

# `git commit-tree` refuses to write a commit without a committer identity, and
# a CI runner has none configured. Supply one for this process so the fixture
# does not depend on the machine's git configuration.
export GIT_AUTHOR_NAME='Gate E fixture'
export GIT_AUTHOR_EMAIL='gate-e@fixture.invalid'
export GIT_COMMITTER_NAME='Gate E fixture'
export GIT_COMMITTER_EMAIL='gate-e@fixture.invalid'

# Gate E reconstructs its revision with `git archive`, so the fixture revision
# must be a real commit object. Writing one from the working tree through a
# throwaway index keeps the test on the sources being changed without touching
# the branch, the real index, or the working tree.
revision="$(
    GIT_INDEX_FILE="$test_root/index" \
        git read-tree HEAD \
        && GIT_INDEX_FILE="$test_root/index" git add -A \
        && GIT_INDEX_FILE="$test_root/index" \
            git commit-tree "$(GIT_INDEX_FILE="$test_root/index" git write-tree)" \
            -p HEAD -m 'Gate E fixture revision'
)"
run_url=https://github.com/meymchen/lspf/actions/runs/5150

# A candidate whose crate is a real archive: Gate E grafts it over
# `crates/lspf`, so the fixture must unpack and identify the revision.
candidate="$test_root/candidate"
evidence="$test_root/candidate-evidence"
create_release_candidate_fixture "$revision" "$candidate" "$evidence"
crate_root="$test_root/crate/lspf-1.0.0"
mkdir -p "$crate_root/src"
jq -n --arg revision "$revision" '{git: {sha1: $revision}}' \
    >"$crate_root/.cargo_vcs_info.json"
printf '// fixture candidate source\n' >"$crate_root/src/lib.rs"
tar -czf "$candidate/lspf-1.0.0.crate" -C "$test_root/crate" lspf-1.0.0
bash ci/prepare-release-candidate.sh "$revision" "$candidate" "$evidence" \
    >/dev/null

fake_cargo="$test_root/fake-cargo"
cat >"$fake_cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ ! -f crates/lspf/.cargo_vcs_info.json ]]; then
    echo 'the candidate crate was not grafted over crates/lspf' >&2
    exit 98
fi

case "$*" in
    *"--test frozen_interface"*) run_id=candidate-compatibility ;;
    *"--test resource_policy"*) run_id=candidate-overload ;;
    *"--test inbound_completion"*) run_id=candidate-timeout ;;
    *"--test client_endpoint"*) run_id=candidate-disconnect ;;
    *"--test stdio_child"*) run_id=candidate-child-cleanup ;;
    *"-p lspf-markdown --all-targets"*) run_id=candidate-reference-server ;;
    install*)
        root=''
        while (($# > 0)); do
            if [[ $1 == --root ]]; then
                root=$2
            fi
            shift
        done
        mkdir -p "$root/bin"
        printf '#!/usr/bin/env bash\n' >"$root/bin/lspf-markdown"
        chmod +x "$root/bin/lspf-markdown"
        cp "$root/bin/lspf-markdown" "$root/bin/lspf-markdown.exe"
        echo 'installed the candidate reference server'
        exit 0
        ;;
    *"--test packaged_editor_journey"*)
        if [[ -z ${LSPF_MARKDOWN_SERVER:-} || ! -x $LSPF_MARKDOWN_SERVER ]]; then
            echo 'the editor journey was not pointed at the installed candidate server' >&2
            exit 97
        fi
        run_id=candidate-editor-journeys
        ;;
    *)
        printf 'unexpected cargo command: %s\n' "$*" >&2
        exit 99
        ;;
esac

if [[ ${FAIL_RUN:-} == "$run_id" ]]; then
    printf 'run %s ... FAILED\n' "$run_id"
    exit 101
fi

printf 'run %s ... ok\n' "$run_id"
EOF
chmod +x "$fake_cargo"

output_dir="$test_root/gate-e"
CARGO_BIN="$fake_cargo" bash ci/run-gate-e-evidence.sh \
    "$revision" "$run_url" "$candidate" "$output_dir" >/dev/null

crate_sha256="$(sha256sum "$candidate/lspf-1.0.0.crate" | awk '{print $1}')"
jq -e \
    --arg revision "$revision" \
    --arg run "$run_url" \
    --arg sha256 "$crate_sha256" '
      .schemaVersion == 1
      and .gate == "E"
      and .revision == $revision
      and .workflowRun == $run
      and .overallResult == "success"
      and .candidate.crate == "lspf"
      and .candidate.version == "1.0.0"
      and .candidate.artifact == "lspf-1.0.0.crate"
      and .candidate.sha256 == $sha256
      and .candidate.graft == "crates/lspf"
      and ([.runs[].id] | sort) == [
        "candidate-child-cleanup",
        "candidate-compatibility",
        "candidate-disconnect",
        "candidate-editor-journeys",
        "candidate-overload",
        "candidate-reference-server",
        "candidate-timeout",
        "release-blocker-register"
      ]
      and all(.runs[]; .result == "success" and (.command | length > 0))
      and ([.validations[].id] | sort) == [
        "candidate-artifact",
        "child-cleanup",
        "compatibility",
        "disconnect",
        "no-undisposed-blocker",
        "overload",
        "timeout"
      ]
      and all(.validations[]; .result == "success" and (.evidence | length > 0))
      and .blockerRegister.result == "success"
      and (.blockerRegister.acceptedFrameworkP0P1 | length == 0)
      and .editorObservations.classification == "human"
      and (.editorObservations.editors | sort) == ["neovim", "vscode", "zed"]
      and (.editorObservations.status | IN("pending", "recorded"))
      and (.editorObservations.recorded | type == "number")
      and (.humanJudgments | length > 0)
      and any(.humanJudgments[]; .statement | contains("user interfaces"))
      and (.failedJourneys | length == 0)
    ' "$output_dir/evidence.json" >/dev/null

grep -F "Revision: [$revision](https://github.com/meymchen/lspf/commit/$revision)" \
    "$output_dir/evidence.md" >/dev/null
grep -F "Passing run: [CI run 5150]($run_url)" "$output_dir/evidence.md" >/dev/null
grep -F "sha256:$crate_sha256" "$output_dir/evidence.md" >/dev/null
grep -F 'Editor journeys on the installed candidate server' \
    "$output_dir/evidence.md" >/dev/null
grep -F '## Editor matrix observations' "$output_dir/evidence.md" >/dev/null
# The recorded command must name the selection that makes the journey validate
# the candidate rather than the workspace build.
jq -e '
  any(.runs[];
    .id == "candidate-editor-journeys"
    and (.command | contains("LSPF_MARKDOWN_SERVER=")))
' "$output_dir/evidence.json" >/dev/null

# The gate must reject a candidate whose crate is not the validated revision.
foreign="$test_root/foreign-candidate"
cp -R "$candidate" "$foreign"
foreign_root="$test_root/foreign-crate/lspf-1.0.0"
mkdir -p "$foreign_root/src"
jq -n '{git: {sha1: "0000000000000000000000000000000000000000"}}' \
    >"$foreign_root/.cargo_vcs_info.json"
printf '// foreign source\n' >"$foreign_root/src/lib.rs"
tar -czf "$foreign/lspf-1.0.0.crate" -C "$test_root/foreign-crate" lspf-1.0.0
if CARGO_BIN="$fake_cargo" bash ci/run-gate-e-evidence.sh \
    "$revision" "$run_url" "$foreign" "$test_root/foreign-gate-e" \
    >"$test_root/foreign.output" 2>&1
then
    echo 'test failure: a candidate crate from another revision passed Gate E' >&2
    exit 1
fi
grep -F 'does not identify the validated revision' "$test_root/foreign.output" \
    >/dev/null

# An undisposed framework-owned P1 blocks the gate rather than the register
# check alone, so the failure reaches the evidence bundle.
open_register="$test_root/open-blockers.json"
jq -n '{
    schemaVersion: 1,
    blockers: [{
      id: "outbound-broker-leak",
      severity: "P1",
      owner: "framework",
      statement: "The outbound broker retains a cancelled request slot.",
      issue: "https://github.com/meymchen/lspf/issues/4242",
      disposition: "open",
      justification: "Reproduced during candidate validation and not yet fixed."
    }]
  }' >"$open_register"
if RELEASE_BLOCKERS_FILE="$open_register" CARGO_BIN="$fake_cargo" \
    bash ci/run-gate-e-evidence.sh \
        "$revision" "$run_url" "$candidate" "$test_root/blocked-gate-e" \
        >"$test_root/blocked.output" 2>&1
then
    echo 'test failure: an undisposed framework P1 passed Gate E' >&2
    exit 1
fi
jq -e '
  .overallResult == "failure"
  and any(.failedJourneys[];
    .id == "release-blocker-register" and .result == "failure")
  and any(.failedJourneys[];
    .id == "no-undisposed-blocker" and .result == "failure")
' "$test_root/blocked-gate-e/evidence.json" >/dev/null
grep -F '## Failing journeys' "$test_root/blocked-gate-e/evidence.md" >/dev/null

# An accepted framework-owned P1 passes but is reported as a maintainer judgment.
accepted_register="$test_root/accepted-blockers.json"
jq -n '{
    schemaVersion: 1,
    blockers: [{
      id: "zed-restart-latency",
      severity: "P1",
      owner: "framework",
      statement: "Restart takes longer than a second on cold caches.",
      issue: "https://github.com/meymchen/lspf/issues/4243",
      disposition: "accepted",
      justification: "Latency only; tracked for the next minor release."
    }]
  }' >"$accepted_register"
RELEASE_BLOCKERS_FILE="$accepted_register" CARGO_BIN="$fake_cargo" \
    bash ci/run-gate-e-evidence.sh \
        "$revision" "$run_url" "$candidate" "$test_root/accepted-gate-e" \
        >/dev/null
jq -e '
  .overallResult == "success"
  and (.blockerRegister.acceptedFrameworkP0P1 | length == 1)
  and any(.blockerRegister.acceptedFrameworkP0P1[]; .id == "zed-restart-latency")
' "$test_root/accepted-gate-e/evidence.json" >/dev/null
grep -F 'zed-restart-latency' "$test_root/accepted-gate-e/evidence.md" >/dev/null

# A register that is not readable JSON must still fail the gate with a complete
# evidence bundle rather than dying while the bundle is being assembled.
printf 'not json at all\n' >"$test_root/broken-blockers.json"
if RELEASE_BLOCKERS_FILE="$test_root/broken-blockers.json" \
    CARGO_BIN="$fake_cargo" bash ci/run-gate-e-evidence.sh \
        "$revision" "$run_url" "$candidate" "$test_root/broken-gate-e" \
        >/dev/null 2>&1
then
    echo 'test failure: an unreadable blocker register passed Gate E' >&2
    exit 1
fi
jq -e '
  .overallResult == "failure"
  and .blockerRegister.result == "failure"
  and (.blockerRegister.recorded == 0)
' "$test_root/broken-gate-e/evidence.json" >/dev/null
grep -F '## Failing journeys' "$test_root/broken-gate-e/evidence.md" >/dev/null

failure_dir="$test_root/failing-gate-e"
if FAIL_RUN=candidate-timeout CARGO_BIN="$fake_cargo" \
    bash ci/run-gate-e-evidence.sh \
        "$revision" "$run_url" "$candidate" "$failure_dir" >/dev/null 2>&1
then
    echo 'test failure: a failing candidate journey produced a successful exit' >&2
    exit 1
fi
jq -e '
  .overallResult == "failure"
  and any(.failedJourneys[]; .id == "candidate-timeout" and .result == "failure")
  and any(.failedJourneys[]; .id == "timeout" and .result == "failure")
' "$failure_dir/evidence.json" >/dev/null
if grep -F 'Passing run:' "$failure_dir/evidence.md" >/dev/null; then
    echo 'test failure: failing evidence labelled its workflow run as passing' >&2
    exit 1
fi

echo 'Gate E evidence runner verified'
