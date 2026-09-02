#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

source ci/test-coverage-schema.sh
source ci/test-coverage-cli.sh
source ci/test-coverage-report.sh

baseline_path=ci/test-coverage-baseline.json
output_path=target/test-coverage/evidence.json

usage() {
    cat <<'EOF'
Usage: bash ci/run-coverage-evidence-tests.sh [--baseline PATH] [--output PATH]

Execute every failure-path test declared by the test-coverage baseline and
write the successfully executed test identifiers as machine-readable evidence.
EOF
}

test_coverage_parse_options \
    test-evidence \
    usage \
    --baseline baseline_path \
    --output output_path \
    -- "$@"

test_coverage_report_bootstrap \
    output_path \
    test-evidence \
    "failure-path test execution did not complete"

[[ -f $baseline_path ]] || test_coverage_report_fail_setup "test-coverage baseline not found: $baseline_path"

if ! test_coverage_baseline_is_valid "$baseline_path" evidence; then
    test_coverage_report_fail_setup "invalid test-coverage evidence declarations: $baseline_path"
fi

mapfile -t evidence_tests < <(jq -r '
    .evidence | to_entries[] | .value[] | [.target, .name] | @tsv
' "$baseline_path" | sort -u)

for evidence_test in "${evidence_tests[@]}"; do
    IFS=$'\t' read -r test_target test_name <<<"$evidence_test"
    test_id="$test_target::$test_name"
    echo "Executing failure-path evidence test: $test_id"
    if ! test_output=$(cargo test -p lspf \
        --features stdio,tcp,websocket \
        --test "$test_target" "$test_name" -- --exact 2>&1); then
        printf '%s\n' "$test_output" >&2
        test_coverage_report_fail_setup "failure-path test failed: $test_id"
    fi
    printf '%s\n' "$test_output"
    case $test_output in
        *'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;'*) ;;
        *) test_coverage_report_fail_setup "failure-path test was not discovered exactly once: $test_id" ;;
    esac
done

jq '{schemaVersion: 1, success: true, tests: .evidence}' \
    "$baseline_path" >"$output_path"
echo "Failure-path test evidence passed: ${#evidence_tests[@]} tests"
