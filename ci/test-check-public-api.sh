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
    info)
        if [[ " $* " == *' --color never '* ]]; then
            echo 'version: 0.5.2'
        else
            printf '\033[1mversion:\033[0m 0.5.2\n'
        fi
        ;;
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
        if [[ " $* " == *websocket* ]]; then
            cat <<OUTPUT
--- failure enum_marked_non_exhaustive: enum marked non-exhaustive ---
       reference: https://example.invalid/enum_marked_non_exhaustive
Failed in:
  struct Context in ${FAKE_CHECKOUT_ROOT:?}/crates/lspf/src/context.rs:18
  enum BuildError in ${FAKE_CHECKOUT_ROOT:?}/crates/lspf/src/error.rs:113
  struct Client in ${FAKE_CHECKOUT_ROOT:?}/crates/lspf/src/client.rs:826
    Finished [   1.000s] lspf
OUTPUT
        else
            cat <<OUTPUT
--- failure enum_marked_non_exhaustive: enum marked non-exhaustive ---
       reference: https://example.invalid/enum_marked_non_exhaustive
Failed in:
  struct Client in ${FAKE_CHECKOUT_ROOT:?}/crates/lspf/src/client.rs:826
  enum BuildError in ${FAKE_CHECKOUT_ROOT:?}/crates/lspf/src/error.rs:113
  struct Context in ${FAKE_CHECKOUT_ROOT:?}/crates/lspf/src/context.rs:18
    Finished [   1.000s] lspf
OUTPUT
        fi
        exit 100
        ;;
    *)
        exit 101
        ;;
esac
EOF
chmod +x "$test_root/bin/cargo"

export PATH="$test_root/bin:$PATH"
export FAKE_CURRENT_VERSION=0.6.0
export FAKE_CHECKOUT_ROOT=/workspace
export CARGO_TERM_COLOR=always

run_gate() {
    (
        cd "$test_root"
        bash ci/check-public-api.sh \
            --baseline-version 0.5.2 \
            --report target/report.json
    )
}

run_gate_with_automatic_baseline() {
    (
        cd "$test_root"
        bash ci/check-public-api.sh \
            --report target/automatic-baseline-report.json
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
    and .intentionalPre1BreakingChanges == false
    and (.rows | length == 4)
    and ([.rows[] | select(.result == "breaking-changes")] | length == 4)
    and ([.rows[].findingsSha256] | unique | length == 1)
    and (.rows[] | select(.target == "native" and .features == "none")
        | .toolExitCode == 100 and .exitCode == 100
          and (.command | contains("--color never"))
          and (.findingsSha256 | test("^[0-9a-f]{64}$")))
    and (.rows[] | select(
            .target == "wasm32-unknown-unknown"
            and .features == "wasm,proposed")
        | (.command | contains("RUSTDOCFLAGS="))
          and ((.command | contains("RUSTFLAGS=")) | not))
'

if run_gate_with_automatic_baseline; then
    echo 'test failure: an unapproved breaking change passed the gate' >&2
    exit 1
fi
jq -e '
    .success == false
    and .baselineVersion == "0.5.2"
    and (.rows | length == 4)
' "$test_root/target/automatic-baseline-report.json" >/dev/null

findings_hash=$(jq -r \
    '.rows[] | select(.target == "native" and .features == "none")
        | .findingsSha256' \
    "$test_root/target/report.json")
jq -n \
    --arg hash "$findings_hash" \
    '{schemaVersion: 1, approvals: [{
        baselineVersion: "0.5.2",
        target: "*", features: "*", findingsSha256: $hash
    }]}' >"$test_root/ci/public-api-breaking-approvals.json"

# The same finding must have the same hash on a different checkout path.
export FAKE_CHECKOUT_ROOT=/home/runner/work/lspf/lspf
run_gate
assert_report '
    .success == true
    and .intentionalPre1BreakingChanges == true
    and ([.rows[] | select(.result == "approved-breaking-changes")]
        | length == 4)
    and (.rows[] | select(.target == "native" and .features == "none")
        | .toolExitCode == 100 and .exitCode == 0)
'

# Windows checkout paths must produce the same approved fingerprint as POSIX
# paths; the fingerprint records findings, not host path separators.
export FAKE_CHECKOUT_ROOT='C:\Users\runner\work\lspf\lspf'
run_gate
assert_report '
    .success == true
    and .intentionalPre1BreakingChanges == true
    and ([.rows[] | select(.result == "approved-breaking-changes")]
        | length == 4)
'

# The manifest version belongs to release-plz, which bumps it in its own
# release pull request. A feature branch therefore still carries the published
# version while it introduces the break, so an approval must hold whatever the
# manifest currently says: the approval records the reviewed findings, not a
# version the branch has no way to know.
export FAKE_CURRENT_VERSION=0.5.2
run_gate
assert_report '
    .success == true
    and .intentionalPre1BreakingChanges == true
    and ([.rows[] | select(.result == "approved-breaking-changes")]
        | length == 4)
'

export FAKE_CURRENT_VERSION=0.6.0
run_gate
assert_report '.success == true'

# An approval still binds to the findings it recorded: a different break under
# the same baseline has a different fingerprint and stays unapproved.
jq '.approvals[0].findingsSha256 |= (.[0:63] + "0")' \
    "$test_root/ci/public-api-breaking-approvals.json" \
    >"$test_root/ci/approvals.tmp"
mv "$test_root/ci/approvals.tmp" "$test_root/ci/public-api-breaking-approvals.json"
export FAKE_CURRENT_VERSION=0.5.2
if run_gate; then
    echo 'test failure: an approval matched findings it does not record' >&2
    exit 1
fi
assert_report '
    .success == false
    and .intentionalPre1BreakingChanges == false
    and ([.rows[] | select(.result == "breaking-changes")] | length == 4)
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
