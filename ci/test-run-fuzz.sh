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

# The matrix the scheduled workflow fans out over is the documented list, not a
# second copy of it that can drift.
listed_targets=$("$runner" --list | awk '{print $1}')
expected_matrix=$(printf '%s\n' "$listed_targets" \
    | jq -Rsc 'split("\n") | map(select(length > 0)) | {target: .}')
actual_matrix=$("$runner" --matrix)
if [[ $actual_matrix != "$expected_matrix" ]]; then
    echo "fuzz matrix does not match the documented target list" >&2
    printf 'expected: %s\nactual:   %s\n' "$expected_matrix" "$actual_matrix" >&2
    exit 1
fi

leg_root="$test_root/legs"
leg_invocations="$test_root/leg-invocations"

if ! FUZZ_CARGO_BIN="$fake_cargo" \
    FUZZ_TEST_ARTIFACT="$artifact_name" \
    FUZZ_TEST_INVOCATIONS="$leg_invocations" \
    "$runner" --target content-length "$leg_root/content-length" >/dev/null
then
    echo "a passing fuzz target leg reported failure" >&2
    exit 1
fi
for recorded in log status duration-ms; do
    if [[ ! -f "$leg_root/content-length/$recorded" ]]; then
        echo "a fuzz target leg did not record $recorded" >&2
        exit 1
    fi
done
if [[ $(cat "$leg_root/content-length/status") != 0 ]]; then
    echo "a passing fuzz target leg recorded a non-zero status" >&2
    exit 1
fi

# A failing leg has to leave its result behind rather than lose it with the
# machine it ran on: the aggregating job reads that, not the exit code.
if FUZZ_CARGO_BIN="$fake_cargo" \
    FUZZ_TEST_ARTIFACT="$artifact_name" \
    FUZZ_TEST_INVOCATIONS="$leg_invocations" \
    "$runner" --target envelope "$leg_root/envelope" >/dev/null
then
    echo "a failing fuzz target leg reported success" >&2
    exit 1
fi
if [[ $(cat "$leg_root/envelope/status") == 0 ]]; then
    echo "a failing fuzz target leg recorded a zero status" >&2
    exit 1
fi
if [[ ! -f "$leg_root/envelope/reproducers/envelope/$artifact_name.minimized" ]]; then
    echo "a failing fuzz target leg did not retain its minimized reproducer" >&2
    exit 1
fi

collect_root="$test_root/collect"
component_dir="$test_root/component"
collected="$test_root/collected.log"
for target in $listed_targets; do
    mkdir -p "$collect_root/$target"
    printf 'replayed log for %s\n' "$target" >"$collect_root/$target/log"
    printf '0\n' >"$collect_root/$target/status"
    printf '1000\n' >"$collect_root/$target/duration-ms"
done
printf '4000\n' >"$collect_root/envelope/duration-ms"
mkdir -p "$collect_root/envelope/reproducers/envelope"
printf 'minimized' \
    >"$collect_root/envelope/reproducers/envelope/$artifact_name.minimized"

if ! "$runner" --collect "$collect_root" "$component_dir" >"$collected"; then
    echo "collecting successful fuzz legs reported failure" >&2
    exit 1
fi
for target in $listed_targets; do
    if ! grep -Fq "replayed log for $target" "$collected"; then
        echo "collected evidence dropped the log for $target" >&2
        exit 1
    fi
done
# The recorded duration is the sweep's wall clock, so it is the longest leg
# rather than the seconds the collecting job spent reading files.
if [[ $(cat "$component_dir/duration-ms") != 4000 ]]; then
    echo "collected fuzz duration is not the sweep's wall clock" >&2
    exit 1
fi
if [[ ! -f "$component_dir/reproducers/envelope/$artifact_name.minimized" ]]; then
    echo "collected evidence dropped a retained reproducer" >&2
    exit 1
fi

printf '1\n' >"$collect_root/uri-identity/status"
if "$runner" --collect "$collect_root" "$test_root/component-failed" >/dev/null; then
    echo "collecting a failed fuzz leg reported success" >&2
    exit 1
fi
printf '0\n' >"$collect_root/uri-identity/status"

rm -rf "${collect_root:?}/notebook-cell-sync"
if "$runner" --collect "$collect_root" "$test_root/component-missing" >/dev/null; then
    echo "collecting an absent fuzz leg reported success" >&2
    exit 1
fi

echo "Fuzz runner contract verified"
