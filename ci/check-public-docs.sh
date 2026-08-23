#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

source ci/supported-feature-matrix.sh

# Deny warnings from both the compiler build and rustdoc. Missing public
# documentation is a crate-level denial, while rustdoc also checks links.
export RUSTFLAGS="${RUSTFLAGS:-} -D warnings"
export RUSTDOCFLAGS="${RUSTDOCFLAGS:-} -D warnings"

document() {
    local target=$1
    local features=${2:-}
    local args=(
        -p lspf
        --no-deps
        --locked
        --target "$target"
        --no-default-features
    )

    if [[ -n $features ]]; then
        args+=(--features "$features")
    fi

    echo "Documenting target=$target features=${features:-none}"
    cargo doc "${args[@]}"
}

# Default is a documented selection in its own right. The loop then mirrors
# every supported native selection from the support contract and builds each
# stable surface again with the orthogonal proposed API enabled.
echo "Documenting native default features"
default_args=(-p lspf --no-deps --locked --target x86_64-unknown-linux-gnu)
cargo doc "${default_args[@]}"
echo "Documenting native default features plus proposed"
cargo doc "${default_args[@]}" --features proposed

document x86_64-unknown-linux-gnu
document x86_64-unknown-linux-gnu proposed

for features in "${NATIVE_FEATURE_SELECTIONS[@]}"
do
    if [[ $features == none ]]; then
        continue
    fi
    document x86_64-unknown-linux-gnu "$features"
    document x86_64-unknown-linux-gnu "$features,proposed"
done

# WASM supports its runtime-only custom-Transport surface and the first-party
# worker-channel adapter. Proposed is independent of both selections.
for features in "${WASM_FEATURE_SELECTIONS[@]}"
do
    document wasm32-unknown-unknown "$features"
    document wasm32-unknown-unknown "$features,proposed"
done

echo "Public API documentation contract verified"
