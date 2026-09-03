#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

package_dir="${1:?usage: check-tutorials.sh PACKAGED_LSP_DIRECTORY}"
package_dir="$(cd "$package_dir" && pwd)"
if command -v cygpath >/dev/null 2>&1; then
    package_dir="$(cygpath -m "$package_dir")"
fi
test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

extract_block() {
    local source=$1
    local marker=$2
    local destination=$3
    local count

    if [[ ! -f $source ]]; then
        printf 'tutorial source does not exist: %s\n' "$source" >&2
        return 1
    fi

    count="$(grep -Fc "<!-- $marker -->" "$source" || true)"
    if [[ $count != 1 ]]; then
        printf '%s must contain exactly one %s marker, found %s\n' \
            "$source" "$marker" "$count" >&2
        return 1
    fi

    awk -v marker="<!-- $marker -->" '
        $0 == marker { after_marker = 1; next }
        after_marker && /^```/ { capture = 1; after_marker = 0; next }
        capture && $0 == "```" { complete = 1; exit }
        capture { print }
        END { if (!complete) exit 1 }
    ' "$source" >"$destination"
}

make_consumer() {
    local name=$1
    local source=$2
    local directory="$test_root/$name"

    local extracted_manifest="$directory/Cargo.extracted.toml"

    mkdir -p "$directory/src"
    extract_block "$source" 'lspf:tutorial-manifest' "$extracted_manifest"
    awk -v dependency="lspf = { path = \"$package_dir\" }" '
        /^lspf = / { print dependency; replaced += 1; next }
        { print }
        END { if (replaced != 1) exit 1 }
    ' "$extracted_manifest" >"$directory/Cargo.toml"
    extract_block "$source" 'lspf:tutorial-program' "$directory/src/main.rs"
}

make_consumer server docs/tutorials/server.md
make_consumer client docs/tutorials/client.md

echo "Locking and building the Server tutorial as a clean consumer"
cargo generate-lockfile --manifest-path "$test_root/server/Cargo.toml"
CARGO_TARGET_DIR="$test_root/server-target" \
    cargo build --manifest-path "$test_root/server/Cargo.toml" --locked

server_binary="$test_root/server-target/debug/lspf-tutorial-server"
if [[ -f "$server_binary.exe" ]]; then
    server_binary="$server_binary.exe"
fi

echo "Locking and building the Client tutorial as a clean consumer"
cargo generate-lockfile --manifest-path "$test_root/client/Cargo.toml"
CARGO_TARGET_DIR="$test_root/client-target" \
    cargo build --manifest-path "$test_root/client/Cargo.toml" --locked

client_binary="$test_root/client-target/debug/lspf-tutorial-client"
if [[ -f "$client_binary.exe" ]]; then
    client_binary="$client_binary.exe"
fi

echo "Running the Client tutorial against the Server tutorial"
"$client_binary" "$server_binary"

echo "Tutorial consumer contract verified"
