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
# every supported native selection from the support contract.
echo "Documenting native default features"
default_args=(-p lspf --no-deps --locked --target x86_64-unknown-linux-gnu)
cargo doc "${default_args[@]}"

document x86_64-unknown-linux-gnu

for features in "${NATIVE_FEATURE_SELECTIONS[@]}"
do
    if [[ $features == none ]]; then
        continue
    fi
    document x86_64-unknown-linux-gnu "$features"
done

# WASM supports its runtime-only custom-Transport surface and the first-party
# worker-channel adapter.
for features in "${WASM_FEATURE_SELECTIONS[@]}"
do
    document wasm32-unknown-unknown "$features"
done

echo "Public API documentation contract verified"
