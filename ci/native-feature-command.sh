#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

source ci/supported-feature-matrix.sh

usage() {
    cat <<'EOF'
Usage: bash ci/native-feature-command.sh COMMAND [ARGS]

Commands:
  matrix                 Print the release-oriented native matrix as JSON
  clippy                 Lint every native lspf target with warnings denied
  test-workspace         Test the workspace with every native feature
  test-workspace-docs    Test workspace documentation with every native feature
  coverage-workspace     Collect workspace coverage without generating a report
  coverage-html          Generate the local HTML coverage report
  test-evidence T N      Run exact integration test N from target T
EOF
}

require_arg_count() {
    local expected=$1
    shift
    if (($# != expected)); then
        usage >&2
        exit 2
    fi
}

command_name=${1:-}
if [[ -z $command_name ]]; then
    usage >&2
    exit 2
fi
shift

case $command_name in
    matrix)
        require_arg_count 0 "$@"
        jq -cn --arg maximal "$MAXIMAL_NATIVE_FEATURES" '{
          include: [
            {name: "default", cargo_args: "", msrv_rustflags: "-D warnings"},
            {name: "stdio",
             cargo_args: "--no-default-features --features stdio",
             msrv_rustflags: "-D warnings"},
            {name: "tcp",
             cargo_args: "--no-default-features --features tcp",
             msrv_rustflags: "-D warnings"},
            {name: "websocket",
             cargo_args: "--no-default-features --features websocket",
             msrv_rustflags: "-D warnings"},
            {name: "all-native-features",
             cargo_args: ("--no-default-features --features " + $maximal),
             msrv_rustflags: "-D warnings"}
          ]
        }'
        ;;
    clippy)
        require_arg_count 0 "$@"
        exec cargo clippy -p lspf --all-targets --locked \
            --no-default-features --features "$MAXIMAL_NATIVE_FEATURES" \
            -- -D warnings
        ;;
    test-workspace)
        require_arg_count 0 "$@"
        exec cargo test --workspace --features "$MAXIMAL_NATIVE_FEATURES" \
            --all-targets
        ;;
    test-workspace-docs)
        require_arg_count 0 "$@"
        exec cargo test --workspace --features "$MAXIMAL_NATIVE_FEATURES" --doc
        ;;
    coverage-workspace)
        require_arg_count 0 "$@"
        exec cargo llvm-cov --workspace --features "$MAXIMAL_NATIVE_FEATURES" \
            --all-targets --no-report
        ;;
    coverage-html)
        require_arg_count 0 "$@"
        exec cargo llvm-cov --features "$NATIVE_TRANSPORT_FEATURES" \
            --all-targets --html --output-dir target/coverage
        ;;
    test-evidence)
        require_arg_count 2 "$@"
        exec cargo test -p lspf --features "$NATIVE_TRANSPORT_FEATURES" \
            --test "$1" "$2" -- --exact
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac
