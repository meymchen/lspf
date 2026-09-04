#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

fake_runner="$test_root/fake-component-runner"
cat >"$fake_runner" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

component=$1
revision=$2
output_dir=$3
mkdir -p "$output_dir"

if [[ ${FAIL_COMPONENT:-} == "$component" ]]; then
    echo "$component produced a reproducible test failure"
    exit 1
fi

# A component whose work ran on other machines - the parallel fuzz sweep -
# reports the span of that work here instead of the time this job spent
# collecting it.
if [[ $component == fuzz ]]; then
    printf '%s\n' 1234567 >"$output_dir/duration-ms"
fi

if [[ $component == performance ]]; then
    cat >"$output_dir/results.json" <<JSON
{
  "revision": "$revision",
  "durationSeconds": 1.25,
  "latencyMs": {"startupP95": 0.1, "requestP95": 0.2, "requestP99": 0.3, "largeDocumentEditP95": 0.4, "largeDocumentEditP99": 0.5},
  "throughputOperationsPerSecond": 90000,
  "peakRssMiB": 15,
  "limitBehavior": {"slowPeer": {"attempted": 128, "accepted": 8, "overloaded": 120, "delivered": 8}},
  "budgetChecks": [{"id": "throughput", "actual": 90000, "result": "success"}],
  "overallResult": "success"
}
JSON
fi

echo "$component passed"
EOF
chmod +x "$fake_runner"

revision=0123456789abcdef0123456789abcdef01234567
run_url=https://github.com/meymchen/lspf/actions/runs/4242
output_dir="$test_root/evidence"

GATE_D_COMPONENT_RUNNER="$fake_runner" bash ci/run-gate-d-evidence.sh \
    "$revision" "$run_url" "$output_dir"

jq -e \
    --arg revision "$revision" \
    --arg run "$run_url" '
      .schemaVersion == 1
      and .gate == "D"
      and .revision == $revision
      and .workflowRun == $run
      and .overallResult == "success"
      and ([.components[].id] | sort) == [
        "editor",
        "fuzz",
        "model",
        "performance",
        "reference-server",
        "soak"
      ]
      and all(.components[];
        .revision == $revision
        and .result == "success"
        and (.configuration | type == "array" and length > 0)
        and all(.configuration[]; contains("/blob/" + $revision + "/"))
        and (.durationMilliseconds | numbers) >= 0
        and (.command | type == "string" and length > 0)
        and (.log | type == "string" and length > 0))
      and (.failedComponents | length == 0)
      and (.components[] | select(.id == "fuzz") | .durationMilliseconds)
        == 1234567
      and .performanceClaims.revision == $revision
      and .performanceClaims.latencyMs.requestP99 == 0.3
      and .performanceClaims.throughputOperationsPerSecond == 90000
      and .performanceClaims.peakRssMiB == 15
      and .performanceClaims.limitBehavior.slowPeer.overloaded == 120
      and .publicInterfaceEvidence.result == "success"
      and .publicInterfaceEvidence.crate == "lspf-markdown"
      and .editorEvidence.classification == "automated"
      and (.humanJudgments | length > 0)
    ' "$output_dir/evidence.json" >/dev/null

for component in fuzz model performance soak reference-server editor; do
    test -f "$output_dir/components/$component/command.log"
    grep -F "Revision: $revision" \
        "$output_dir/components/$component/command.log" >/dev/null
    grep -F 'Configuration:' \
        "$output_dir/components/$component/command.log" >/dev/null
    grep -F 'Duration milliseconds:' \
        "$output_dir/components/$component/command.log" >/dev/null
    jq -e --arg revision "$revision" '
      .revision == $revision
      and (.configuration | type == "array" and length > 0)
      and (.durationMilliseconds | numbers) >= 0
      and (.result == "success")
    ' "$output_dir/components/$component/metadata.json" >/dev/null
done

grep -F "Revision: [$revision](https://github.com/meymchen/lspf/commit/$revision)" \
    "$output_dir/evidence.md" >/dev/null
grep -F "Passing run: [CI run 4242]($run_url)" "$output_dir/evidence.md" >/dev/null
grep -F 'Request p99 latency: `0.3 ms`' "$output_dir/evidence.md" >/dev/null
grep -F 'Configuration: [revision-locked source]' \
    "$output_dir/evidence.md" >/dev/null
grep -F '## Human judgments' "$output_dir/evidence.md" >/dev/null

failure_dir="$test_root/failing-evidence"
if FAIL_COMPONENT=model GATE_D_COMPONENT_RUNNER="$fake_runner" \
    bash ci/run-gate-d-evidence.sh "$revision" "$run_url" "$failure_dir"
then
    echo 'test failure: a failing model produced a successful Gate D exit' >&2
    exit 1
fi

jq -e '
  .overallResult == "failure"
  and ([.failedComponents[].id] | sort) == ["model"]
  and all(.failedComponents[];
    .result == "failure"
    and .failureAnalysis == "requires-analysis"
    and (.explanation | type == "string" and length > 0)
    and (.log | type == "string" and length > 0))
  and any(.components[];
    .id == "model" and .result == "failure"
    and .failureAnalysis == "requires-analysis"
    and (.explanation | contains("model produced a reproducible test failure")))
' "$failure_dir/evidence.json" >/dev/null
grep -F '## Failing components' "$failure_dir/evidence.md" >/dev/null
grep -F 'Model interleavings: `failure`' "$failure_dir/evidence.md" >/dev/null
if grep -F 'Passing run:' "$failure_dir/evidence.md" >/dev/null; then
    echo 'test failure: failing Gate D evidence labelled its run as passing' >&2
    exit 1
fi

echo 'Gate D evidence runner verified'
