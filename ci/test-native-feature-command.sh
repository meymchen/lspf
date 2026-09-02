#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

source ci/supported-feature-matrix.sh

test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT
mkdir -p "$test_root/bin"

cat >"$test_root/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >"${CAPTURE_PATH:?}"
EOF
chmod +x "$test_root/bin/cargo"

assert_command() {
    local command_name=$1
    local expected=$2
    shift 2
    local capture_path="$test_root/$command_name"
    local actual

    PATH="$test_root/bin:$PATH" CAPTURE_PATH="$capture_path" \
        bash ci/native-feature-command.sh "$command_name" "$@"
    actual=$(<"$capture_path")
    if [[ $actual != "$expected" ]]; then
        printf 'unexpected %s command\nexpected: %s\nactual:   %s\n' \
            "$command_name" "$expected" "$actual" >&2
        exit 1
    fi
}

matrix=$(bash ci/native-feature-command.sh matrix)
jq -e --arg maximal "$MAXIMAL_NATIVE_FEATURES" '
    .include | length == 5
    and any(.[];
        .name == "all-native-features"
        and .cargo_args == ("--no-default-features --features " + $maximal))
' <<<"$matrix" >/dev/null

assert_command clippy \
    "clippy -p lspf --all-targets --locked --no-default-features --features $MAXIMAL_NATIVE_FEATURES -- -D warnings"
assert_command test-workspace \
    "test --workspace --features $MAXIMAL_NATIVE_FEATURES --all-targets"
assert_command test-workspace-docs \
    "test --workspace --features $MAXIMAL_NATIVE_FEATURES --doc"
assert_command coverage-workspace \
    "llvm-cov --workspace --features $MAXIMAL_NATIVE_FEATURES --all-targets --no-report"
assert_command coverage-html \
    "llvm-cov --features $NATIVE_TRANSPORT_FEATURES --all-targets --html --output-dir target/coverage"
assert_command test-evidence \
    "test -p lspf --features $NATIVE_TRANSPORT_FEATURES --test diagnostics one_test -- --exact" \
    diagnostics one_test

native_test_alias=$(sed -n 's/^test-native = "\(.*\)"/\1/p' .cargo/config.toml)
if [[ $native_test_alias != \
    "test --workspace --features $MAXIMAL_NATIVE_FEATURES --all-targets" ]]
then
    echo "cargo test-native drifted from the canonical maximal feature set" >&2
    exit 1
fi

coverage_alias=$(sed -n 's/^coverage = "\(.*\)"/\1/p' .cargo/config.toml)
if [[ $coverage_alias != \
    "llvm-cov --features $NATIVE_TRANSPORT_FEATURES --all-targets --html --output-dir target/coverage" ]]
then
    echo "cargo coverage drifted from the canonical native feature set" >&2
    exit 1
fi

if bash ci/native-feature-command.sh test-evidence diagnostics >/dev/null 2>&1; then
    echo "test-evidence accepted a missing test name" >&2
    exit 1
fi

echo "Native feature command contract verified"
