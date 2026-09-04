#!/usr/bin/env bash

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
runner="$repo_root/ci/run-fuzz.sh"

expected=$(cat <<'EOF'
envelope 65536 5 300
content-length 65536 5 300
uri-identity 4096 5 300
position-conversion 65536 5 300
incremental-edits 65536 5 300
notebook-cell-sync 4096 5 300
lifecycle-sequences 16384 10 300
EOF
)

actual=$("$runner" --list)

if [[ $actual != "$expected" ]]; then
    echo "fuzz target configuration does not match the documented contract" >&2
    diff -u <(printf '%s\n' "$expected") <(printf '%s\n' "$actual") >&2 || true
    exit 1
fi

"$runner" --check

test_root=$(mktemp -d)
artifact_name="crash-contract-$$"
artifact_root="$test_root/artifacts"
artifact_path="$artifact_root/envelope/$artifact_name"
trap 'rm -rf "$test_root"' EXIT
fake_cargo="$test_root/cargo"
invocations="$test_root/invocations"

cat >"$fake_cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$FUZZ_TEST_INVOCATIONS"
if [[ $* == *" fuzz run envelope "* ]]; then
    artifact_prefix=${*: -1}
    artifact_prefix=${artifact_prefix#-artifact_prefix=}
    printf 'failure' >"${artifact_prefix}${FUZZ_TEST_ARTIFACT}"
    exit 1
fi
if [[ $* == *" fuzz tmin envelope "* ]]; then
    exact_path=${*: -1}
    exact_path=${exact_path#-exact_artifact_path=}
    printf 'minimized' >"$exact_path"
fi
EOF
chmod +x "$fake_cargo"

if FUZZ_CARGO_BIN="$fake_cargo" \
    FUZZ_ARTIFACT_ROOT="$artifact_root" \
    FUZZ_TEST_ARTIFACT="$artifact_name" \
    FUZZ_TEST_INVOCATIONS="$invocations" \
    "$runner" --all
then
    echo "fuzz runner should report an aggregate failure" >&2
    exit 1
fi

run_count=$(grep -c ' fuzz run ' "$invocations")
if [[ $run_count != 7 ]]; then
    echo "fuzz runner stopped before executing every target" >&2
    exit 1
fi
if ! grep -q ' fuzz tmin envelope ' "$invocations"; then
    echo "fuzz runner did not minimize the failing target" >&2
    exit 1
fi
if [[ ! -f "$artifact_path.minimized" ]]; then
    echo "fuzz runner did not retain the minimized reproducer" >&2
    exit 1
fi

echo "Fuzz runner contract verified"
