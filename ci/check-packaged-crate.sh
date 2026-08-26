#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

crate_name=lspf
test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT

package_list="$test_root/package-list.txt"
archive_list="$test_root/archive-list.txt"

echo "Checking the files Cargo will include in the package"
cargo package -p "$crate_name" --list --locked --allow-dirty | sort >"$package_list"
bash ci/check-package-file-policy.sh "$package_list"

echo "Building and verifying the package"
cargo package -p "$crate_name" --locked --allow-dirty

crate_version=$(cargo metadata --no-deps --format-version 1 \
    | jq -er --arg name "$crate_name" \
        '.packages[] | select(.name == $name) | .version')
archive="target/package/$crate_name-$crate_version.crate"
package_dir="$test_root/$crate_name-$crate_version"

tar -xzf "$archive" -C "$test_root"
tar -tzf "$archive" \
    | sed "s#^$crate_name-$crate_version/##" \
    | sort >"$archive_list"
diff -u "$package_list" "$archive_list"

echo "Building documentation from the extracted package"
RUSTFLAGS="${RUSTFLAGS:-} -D warnings" \
RUSTDOCFLAGS="${RUSTDOCFLAGS:-} -D warnings" \
CARGO_TARGET_DIR="$test_root/doc-target" \
    cargo doc \
        --manifest-path "$package_dir/Cargo.toml" \
        --no-deps \
        --locked

consumer_dir="$test_root/consumer"
mkdir -p "$consumer_dir/src"

cat >"$consumer_dir/Cargo.toml" <<EOF
[package]
name = "packaged-lspf-consumer"
version = "0.0.0"
edition = "2024"

[dependencies]
lspf = { path = "$package_dir", default-features = false, features = ["stdio"] }
tokio = { version = "1", features = ["rt-multi-thread"] }
EOF

cat >"$consumer_dir/src/main.rs" <<'EOF'
use lspf::Server;

fn main() {
    let runtime = tokio_runtime();
    let server = Server::builder(())
        .build()
        .expect("the empty server is valid");
    let outcome = runtime
        .block_on(lspf::stdio(server).serve())
        .expect("the stdio lifecycle completes");
    std::process::exit(outcome.code());
}

fn tokio_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the Tokio runtime starts")
}
EOF

echo "Compiling a clean external consumer"
cargo generate-lockfile --manifest-path "$consumer_dir/Cargo.toml"
CARGO_TARGET_DIR="$test_root/consumer-target" \
    cargo build --manifest-path "$consumer_dir/Cargo.toml" --locked

frame() {
    local body=$1
    printf 'Content-Length: %d\r\n\r\n%s' "${#body}" "$body"
}

echo "Running a complete stdio Server lifecycle from the external consumer"
lifecycle_output=$(
    {
        frame '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}'
        frame '{"jsonrpc":"2.0","method":"initialized","params":{}}'
        frame '{"jsonrpc":"2.0","id":2,"method":"shutdown"}'
        frame '{"jsonrpc":"2.0","method":"exit"}'
    } | "$test_root/consumer-target/debug/packaged-lspf-consumer"
)

if [[ $lifecycle_output != *'"id":1'* || $lifecycle_output != *'"id":2'* ]]; then
    echo "the packaged consumer did not return both lifecycle responses" >&2
    exit 1
fi

echo "Packaged crate contract verified"
