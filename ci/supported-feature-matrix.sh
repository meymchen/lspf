#!/usr/bin/env bash

# Supported no-default-feature selections. Keep target-specific additions in
# these arrays; the feature-contract and public-docs gates both consume them.
readonly -a NATIVE_FEATURE_SELECTIONS=(
    none
    runtime-tokio
    stdio
    tcp
    websocket
    stdio,tcp
    stdio,websocket
    tcp,websocket
    stdio,tcp,websocket
)

readonly -a WASM_FEATURE_SELECTIONS=(
    wasm
    worker-channel
)
