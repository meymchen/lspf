#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"; rm -f target/package/lspf-fixture-0.0.0.crate' EXIT

revision="$(git rev-parse HEAD)"
crate_file=lspf-fixture-0.0.0.crate
candidate="$test_root/$crate_file"
printf 'the validated candidate bytes\n' >"$candidate"

# The fake packager stands in for `cargo package`, so the test can decide
# whether this revision still reproduces the candidate.
fake_cargo="$test_root/fake-cargo"
cat >"$fake_cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$*" != "package -p lspf --locked" ]]; then
    printf 'unexpected cargo command: %s\n' "$*" >&2
    exit 99
fi

mkdir -p target/package
printf '%s\n' "${PACKAGED_BYTES:?PACKAGED_BYTES must be set}" \
    >"target/package/$CRATE_FILE"
EOF
chmod +x "$fake_cargo"

CARGO_BIN="$fake_cargo" \
    CRATE_FILE="$crate_file" \
    PACKAGED_BYTES='the validated candidate bytes' \
    bash ci/check-repackaged-candidate.sh "$revision" "$candidate" \
    | grep -F 'reproduces the validated candidate' >/dev/null

if CARGO_BIN="$fake_cargo" \
    CRATE_FILE="$crate_file" \
    PACKAGED_BYTES='drifted since the candidate was built' \
    bash ci/check-repackaged-candidate.sh "$revision" "$candidate" \
    >"$test_root/drift.output" 2>&1
then
    echo 'test failure: a revision that no longer repackages to the candidate was published' >&2
    exit 1
fi
grep -F 'refusing to publish' "$test_root/drift.output" >/dev/null

if CARGO_BIN="$fake_cargo" CRATE_FILE="$crate_file" \
    bash ci/check-repackaged-candidate.sh \
    0000000000000000000000000000000000000000 "$candidate" \
    >"$test_root/revision.output" 2>&1
then
    echo 'test failure: a revision other than the checked-out one was published' >&2
    exit 1
fi
grep -F 'does not match checked-out revision' "$test_root/revision.output" \
    >/dev/null

if CARGO_BIN="$fake_cargo" CRATE_FILE="$crate_file" \
    bash ci/check-repackaged-candidate.sh "$revision" "$test_root/absent.crate" \
    >"$test_root/absent.output" 2>&1
then
    echo 'test failure: an absent candidate crate passed the repackaging check' >&2
    exit 1
fi
grep -F 'candidate crate is missing or empty' "$test_root/absent.output" \
    >/dev/null

echo 'Repackaged candidate comparison verified'
