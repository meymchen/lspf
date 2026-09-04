#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

source ci/supported-feature-matrix.sh

bash ci/tests/unit/native-feature-command.sh

fail() {
    echo "feature-contract error: $*" >&2
    exit 1
}

assert_excludes() {
    local graph=$1
    local forbidden=$2
    local explanation=$3

    if [[ $graph == *"$forbidden"* ]]; then
        fail "$explanation; found '$forbidden' in the Cargo feature graph"
    fi
}

assert_includes() {
    local graph=$1
    local required=$2
    local explanation=$3

    if [[ $graph != *"$required"* ]]; then
        fail "$explanation; '$required' is absent from the Cargo feature graph"
    fi
}

expect_compile_error() {
    local expected=$1
    shift
    local output

    if output=$("$@" 2>&1); then
        fail "unsupported combination unexpectedly compiled: $*"
    fi
    if [[ $output != *"$expected"* ]]; then
        echo "$output" >&2
        fail "unsupported combination did not report the expected diagnostic: $expected"
    fi
}

package_graph() {
    cargo tree -p lspf --locked -e normal --prefix none --format '{p}' --no-dedupe "$@" \
        | LC_ALL=C sort -u
}

package_names() {
    cargo tree -p lspf --locked -e normal --prefix none --format '{lib}' --no-dedupe "$@" \
        | LC_ALL=C sort -u
}

assert_no_package_overlap() {
    local left=$1
    local right=$2
    local explanation=$3
    local overlap

    overlap=$(comm -12 <(printf '%s\n' "$left") <(printf '%s\n' "$right"))
    if [[ -n $overlap ]]; then
        fail "$explanation; leaked package(s): ${overlap//$'\n'/, }"
    fi
}

default_packages=$(package_graph)
# Keep the checked-in allowlist deterministic across developer hosts. CI's
# authoritative native environment is Linux, so resolve this snapshot for the
# same target even when the contract is run from Windows or macOS.
default_package_names=$(package_names --target x86_64-unknown-linux-gnu)
stdio_packages=$(package_graph --no-default-features --features stdio)
tcp_only_packages=$(comm -13 \
    <(printf '%s\n' "$stdio_packages") \
    <(package_graph --no-default-features --features tcp))
websocket_only_packages=$(comm -13 \
    <(printf '%s\n' "$stdio_packages") \
    <(package_graph --no-default-features --features websocket))
wasm_core_packages=$(package_graph --target wasm32-unknown-unknown --no-default-features)
wasm_only_packages=$(comm -13 \
    <(printf '%s\n' "$wasm_core_packages") \
    <(package_graph --target wasm32-unknown-unknown --no-default-features --features wasm))
worker_only_packages=$(comm -13 \
    <(package_graph --target wasm32-unknown-unknown --no-default-features --features wasm) \
    <(package_graph --target wasm32-unknown-unknown --no-default-features --features worker-channel))

if [[ $default_packages != "$stdio_packages" ]]; then
    fail "the native default dependency graph must equal explicit stdio"
fi
if ! diff -u ci/policy/default-dependencies.txt <(printf '%s\n' "$default_package_names"); then
    fail "the native default dependency allowlist changed; verify no Transport- or WASM-only package leaked, then update ci/policy/default-dependencies.txt"
fi
for exclusive_packages in \
    "$tcp_only_packages" \
    "$websocket_only_packages" \
    "$wasm_only_packages" \
    "$worker_only_packages"
do
    assert_no_package_overlap \
        "$default_packages" \
        "$exclusive_packages" \
        "the native default must exclude TCP-, WebSocket-, and WASM-only dependencies"
done

worker_graph=$(cargo tree \
    -p lspf \
    --locked \
    --target wasm32-unknown-unknown \
    --no-default-features \
    --features worker-channel \
    -e normal,features \
    --prefix none)
worker_features=$(cargo tree \
    -p lspf \
    --locked \
    --target wasm32-unknown-unknown \
    --no-default-features \
    --features worker-channel \
    -e normal,features \
    -i lspf \
    --prefix none)

assert_includes \
    "$worker_features" \
    'lspf feature "wasm"' \
    "worker-channel must include wasm"
for runtime_feature in \
    'lspf feature "runtime-tokio"' \
    'tokio feature "macros"' \
    'tokio feature "rt"' \
    'tokio feature "rt-multi-thread"' \
    'tokio-macros v'
do
    assert_excludes \
        "$worker_graph$worker_features" \
        "$runtime_feature" \
        "the WASM-only build must not enable the Tokio runtime"
done

# Compile every supported selection here. This is deliberately more exhaustive
# than the test matrix: this gate owns the public target/feature contract.
for features in "${NATIVE_FEATURE_SELECTIONS[@]}"
do
    if [[ $features == none ]]; then
        cargo check -p lspf --locked --no-default-features
    else
        cargo check -p lspf --locked --no-default-features --features "$features"
    fi
done
for features in "${WASM_FEATURE_SELECTIONS[@]}"
do
    cargo check -p lspf --locked --target wasm32-unknown-unknown \
        --no-default-features --features "$features"
done

expect_compile_error \
    'the `worker-channel` feature requires the wasm32 target' \
    cargo check -p lspf --locked --no-default-features --features worker-channel
expect_compile_error \
    'the wasm32 target requires the `wasm` feature' \
    cargo check -p lspf --locked --target wasm32-unknown-unknown \
    --no-default-features
expect_compile_error \
    'the `tcp` feature is not supported on the wasm32 target' \
    cargo check -p lspf --locked --target wasm32-unknown-unknown \
    --no-default-features --features wasm,tcp
expect_compile_error \
    'the `websocket` feature is not supported on the wasm32 target' \
    cargo check -p lspf --locked --target wasm32-unknown-unknown \
    --no-default-features --features wasm,websocket
expect_compile_error \
    'the `testing` feature requires a native target' \
    cargo check -p lspf --locked --target wasm32-unknown-unknown \
    --no-default-features --features wasm,testing

echo "Cargo feature and target contract verified"
