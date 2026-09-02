#!/usr/bin/env bash

readonly NATIVE_TRANSPORT_FEATURES=stdio,tcp,websocket
readonly MAXIMAL_NATIVE_FEATURES="$NATIVE_TRANSPORT_FEATURES,testing"

# Supported no-default-feature selections. Keep target-specific additions in
# these arrays; the feature-contract and public-docs gates both consume them.
readonly -a NATIVE_FEATURE_SELECTIONS=(
    none
    runtime-tokio
    stdio
    tcp
    websocket
    testing
    stdio,testing
    tcp,testing
    websocket,testing
    stdio,tcp
    stdio,websocket
    tcp,websocket
    stdio,tcp,testing
    stdio,websocket,testing
    tcp,websocket,testing
    "$NATIVE_TRANSPORT_FEATURES"
    "$MAXIMAL_NATIVE_FEATURES"
)

readonly -a WASM_FEATURE_SELECTIONS=(
    wasm
    worker-channel
)

# Representative public API surfaces. The exhaustive compile and rustdoc
# matrices above own feature-combination correctness; semver checking needs
# only the core surface and the additive maximal surface for each target.
readonly -a PUBLIC_API_SURFACES=(
    'native|none'
    "native|$MAXIMAL_NATIVE_FEATURES"
    'wasm32-unknown-unknown|wasm'
    'wasm32-unknown-unknown|worker-channel'
)
