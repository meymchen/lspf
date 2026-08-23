#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT

mkdir -p "$test_root/bin" "$test_root/ci"
cp ci/check-public-api.sh ci/supported-feature-matrix.sh "$test_root/ci/"
cp ci/public-api-breaking-approvals.json "$test_root/ci/"

cat >"$test_root/bin/cargo" <<'EOF'
#!/usr/bin/env bash

set -euo pipefail

case ${1:-} in
    metadata)
        jq -cn --arg version "${FAKE_CURRENT_VERSION:?}" \
            '{packages: [{name: "lspf", version: $version}]}'
        ;;
    semver-checks)
        if [[ ${2:-} == --version ]]; then
            echo 'cargo-semver-checks 0.50.0'
            exit 0
        fi
        if [[ ${2:-} != check-release ]]; then
            exit 101
        fi
        if [[ " $* " != *' --color never '* ]]; then
            echo 'test fixture requires deterministic, uncolored output' >&2
            exit 101
        fi
        if [[ " $* " != *' --features '* \
            && ${RUSTFLAGS:-} != *'target_arch="wasm32"'* ]]; then
            cat <<'OUTPUT'
--- failure function_missing: pub function removed ---
       reference: https://example.invalid/function_missing
       baseline: file /tmp/registry/lspf-0.5.2/src/lib.rs:10
       current: file /workspace/crates/lspf/src/lib.rs:10
    Finished [   1.000s] lspf
OUTPUT
            exit 100
        fi
        echo 'Completed [   1.000s] lspf'
        ;;
    *)
        exit 101
        ;;
esac
EOF
chmod +x "$test_root/bin/cargo"

export PATH="$test_root/bin:$PATH"
export FAKE_CURRENT_VERSION=0.6.0
export CARGO_TERM_COLOR=always

run_gate() {
    (
        cd "$test_root"
        bash ci/check-public-api.sh \
            --baseline-version 0.5.2 \
            --report target/report.json
    )
}

assert_report() {
    local expression=$1
    jq -e "$expression" "$test_root/target/report.json" >/dev/null
}

if run_gate; then
    echo 'test failure: an unapproved breaking change passed the gate' >&2
    exit 1
fi
assert_report '
    .schemaVersion == 1
    and .success == false
    and .releaseType == "minor"
    and .intentionalPre1BreakingChanges == false
    and (.rows | length == 22)
    and ([.rows[] | select(.result == "breaking-changes")] | length == 1)
    and (.rows[] | select(.target == "native" and .features == "none")
        | .toolExitCode == 100 and .exitCode == 100
          and (.command | contains("--color never"))
          and (.findingsSha256 | test("^[0-9a-f]{64}$")))
'

findings_hash=$(jq -r \
    '.rows[] | select(.target == "native" and .features == "none")
        | .findingsSha256' \
    "$test_root/target/report.json")
jq -n \
    --arg hash "$findings_hash" \
    '{schemaVersion: 1, approvals: [{
        baselineVersion: "0.5.2", currentVersion: "0.6.0",
        target: "native", features: "none", findingsSha256: $hash
    }]}' >"$test_root/ci/public-api-breaking-approvals.json"

run_gate
assert_report '
    .success == true
    and .intentionalPre1BreakingChanges == true
    and ([.rows[] | select(.result == "approved-breaking-changes")]
        | length == 1)
    and (.rows[] | select(.target == "native" and .features == "none")
        | .toolExitCode == 100 and .exitCode == 0)
'

export FAKE_CURRENT_VERSION=0.5.3
if run_gate; then
    echo 'test failure: a patch release accepted a breaking-change approval' >&2
    exit 1
fi
assert_report '
    .success == false
    and .releaseType == "patch"
    and .intentionalPre1BreakingChanges == false
    and ([.rows[] | select(.result == "breaking-changes")] | length == 1)
'

echo '{"schemaVersion":1,"approvals":"invalid"}' \
    >"$test_root/ci/public-api-breaking-approvals.json"
if run_gate; then
    echo 'test failure: an invalid approval registry passed the gate' >&2
    exit 1
fi
assert_report '
    .success == false
    and .rows == []
    and (.setupError | contains("invalid breaking-change approval registry"))
'

echo 'Public API compatibility gate tests passed'
