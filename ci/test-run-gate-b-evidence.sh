#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

fake_cargo="$test_root/fake-cargo"
cat >"$fake_cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

test_name="${6:?missing exact test name}"
if [[ $test_name == fixed_budget_floods_and_a_slow_reader_never_exceed_connection_limits ]]; then
    printf '%s\n' '{
      "resources": [
        {"name":"inbound_requests","limit":2,"observedPeak":2},
        {"name":"handler_tasks","limit":2,"observedPeak":2},
        {"name":"outbound_messages","limit":8,"observedPeak":1},
        {"name":"outbound_bytes","limit":16384,"observedPeak":12345},
        {"name":"documents","limit":2,"observedPeak":2},
        {"name":"document_bytes","limit":8,"observedPeak":8}
      ]
    }' >"${LSPF_GATE_B_OBSERVATIONS:?missing observation path}"
fi

if [[ ${FAIL_TEST:-} == "$test_name" ]]; then
    printf 'test %s ... FAILED\n' "$test_name"
    exit 101
fi

printf 'test %s ... ok\n' "$test_name"
EOF
chmod +x "$fake_cargo"

revision=0123456789abcdef0123456789abcdef01234567
run_url=https://github.com/meymchen/lspf/actions/runs/4242
output_dir="$test_root/evidence"

CARGO_BIN="$fake_cargo" bash ci/run-gate-b-evidence.sh \
    "$revision" "$run_url" "$output_dir"

jq -e \
    --arg revision "$revision" \
    --arg run "$run_url" '
      .schemaVersion == 1
      and .gate == "B"
      and .revision == $revision
      and .workflowRun == $run
      and .overallResult == "success"
      and ([.scenarios[].id] | sort) == [
        "cancellation",
        "disconnect",
        "flood",
        "shutdown",
        "slow-peer",
        "stalled-handler",
        "timeout"
      ]
      and all(.scenarios[]; .result == "success" and (.command | length > 0))
      and ([.resources[].name] | sort) == [
        "document_bytes",
        "documents",
        "handler_tasks",
        "inbound_requests",
        "outbound_bytes",
        "outbound_messages"
      ]
      and all(.resources[]; .observedPeak <= .limit)
      and (.invariants | length == 3)
      and all(.invariants[]; .result == "success" and (.evidence | length > 0))
      and (.failedChecks | length == 0)
    ' "$output_dir/evidence.json" >/dev/null

grep -F "Revision: [$revision](https://github.com/meymchen/lspf/commit/$revision)" \
    "$output_dir/evidence.md" >/dev/null
grep -F "Passing run: [CI run 4242]($run_url)" "$output_dir/evidence.md" >/dev/null
grep -F '| `outbound_bytes` | 16384 | 12345 |' "$output_dir/evidence.md" >/dev/null

failure_dir="$test_root/failing-evidence"
if FAIL_TEST=handler_timeout_completes_the_request_exactly_once \
    CARGO_BIN="$fake_cargo" bash ci/run-gate-b-evidence.sh \
        "$revision" "$run_url" "$failure_dir"
then
    echo 'test failure: a failing scenario produced a successful exit' >&2
    exit 1
fi

jq -e '
  .overallResult == "failure"
  and any(.failedChecks[];
    .id == "stalled-handler" and .result == "failure")
  and any(.failedChecks[];
    .id == "timeout" and .result == "failure")
' "$failure_dir/evidence.json" >/dev/null
grep -F '## Failing checks' "$failure_dir/evidence.md" >/dev/null
if grep -F 'Passing run:' "$failure_dir/evidence.md" >/dev/null; then
    echo 'test failure: failing evidence labelled its workflow run as passing' >&2
    exit 1
fi

echo 'Gate B evidence runner verified'
