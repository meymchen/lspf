#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

fake_cargo="$test_root/fake-cargo"
cat >"$fake_cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

case "$*" in
    *"--test public_conformance"*) run_id=public-conformance ;;
    *"--doc markdown::ClientAdoptionGuide"*) run_id=client-adoption-doctests ;;
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

revision=0123456789abcdef0123456789abcdef01234567
run_url=https://github.com/meymchen/lspf/actions/runs/4242
output_dir="$test_root/evidence"

CARGO_BIN="$fake_cargo" bash ci/run-gate-c-evidence.sh \
    "$revision" "$run_url" "$output_dir"

jq -e \
    --arg revision "$revision" \
    --arg run "$run_url" '
      .schemaVersion == 1
      and .gate == "C"
      and .revision == $revision
      and .workflowRun == $run
      and .overallResult == "success"
      and ([.runs[].id] | sort) == [
        "client-adoption-doctests",
        "public-conformance"
      ]
      and all(.runs[]; .result == "success" and (.command | length > 0))
      and ([.journeys[].id] | sort) == ["custom-transport", "stdio-child"]
      and any(.journeys[];
        .id == "custom-transport"
        and (.represented | sort) == [
          "cancellation",
          "initialize",
          "reverse-calls",
          "shutdown",
          "timeout",
          "transport-failure",
          "typed-calls"
        ])
      and any(.journeys[];
        .id == "stdio-child"
        and (.represented | sort) == [
          "abnormal-exit",
          "initialize",
          "reverse-calls",
          "shutdown",
          "stderr-drain",
          "typed-calls"
        ])
      and ([.invariants[].id] | sort) == [
        "child-reaped",
        "pending-work-resolved",
        "public-only"
      ]
      and all(.invariants[]; .result == "success" and (.evidence | length > 0))
      and (.failedChecks | length == 0)
    ' "$output_dir/evidence.json" >/dev/null

grep -F "Revision: [$revision](https://github.com/meymchen/lspf/commit/$revision)" \
    "$output_dir/evidence.md" >/dev/null
grep -F "Passing run: [CI run 4242]($run_url)" "$output_dir/evidence.md" >/dev/null
grep -F 'Custom Transport' "$output_dir/evidence.md" >/dev/null
grep -F 'Real stdio child' "$output_dir/evidence.md" >/dev/null

relative_dir="$(realpath --relative-to="$PWD" "$test_root/relative-evidence")"
CARGO_BIN="$fake_cargo" bash ci/run-gate-c-evidence.sh \
    "$revision" "$run_url" "$relative_dir"
jq -e '.overallResult == "success"' "$relative_dir/evidence.json" >/dev/null

failure_dir="$test_root/failing-evidence"
if FAIL_RUN=public-conformance \
    CARGO_BIN="$fake_cargo" bash ci/run-gate-c-evidence.sh \
        "$revision" "$run_url" "$failure_dir"
then
    echo 'test failure: a failing journey produced a successful exit' >&2
    exit 1
fi

jq -e '
  .overallResult == "failure"
  and any(.failedChecks[];
    .id == "custom-transport" and .result == "failure")
  and any(.failedChecks[];
    .id == "stdio-child" and .result == "failure")
  and any(.failedChecks[];
    .id == "pending-work-resolved" and .result == "failure")
' "$failure_dir/evidence.json" >/dev/null
grep -F '## Failing checks' "$failure_dir/evidence.md" >/dev/null
if grep -F 'Passing run:' "$failure_dir/evidence.md" >/dev/null; then
    echo 'test failure: failing evidence labelled its workflow run as passing' >&2
    exit 1
fi

echo 'Gate C evidence runner verified'
