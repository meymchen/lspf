#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../.."

test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT

valid_list="$test_root/valid.txt"
cat >"$valid_list" <<'EOF'
.cargo_vcs_info.json
CHANGELOG.md
Cargo.lock
Cargo.toml
Cargo.toml.orig
README.md
examples/demo.rs
src/lib.rs
src/new_module.rs
tests/new_behavior.rs
EOF

# New files within an intentional package area must not require snapshot
# maintenance.
bash ci/check-package-file-policy.sh "$valid_list"

assert_rejected() {
    local path=$1
    local expected=$2
    local candidate="$test_root/candidate.txt"
    local output="$test_root/output.txt"

    cp "$valid_list" "$candidate"
    printf '%s\n' "$path" >>"$candidate"
    if bash ci/check-package-file-policy.sh "$candidate" >"$output" 2>&1; then
        echo "package policy accepted forbidden path: $path" >&2
        exit 1
    fi
    if ! grep -Fq "$expected" "$output"; then
        echo "package policy reported the wrong failure for: $path" >&2
        cat "$output" >&2
        exit 1
    fi
}

assert_rejected "target/debug/lspf" "forbidden package path"
assert_rejected "examples/demo/private.pem" "forbidden package path"
assert_rejected "release-notes.txt" "unexpected package path"

missing_required="$test_root/missing-required.txt"
grep -Fvx 'src/lib.rs' "$valid_list" >"$missing_required"
if bash ci/check-package-file-policy.sh "$missing_required" \
    >"$test_root/missing-output.txt" 2>&1; then
    echo "package policy accepted a list without src/lib.rs" >&2
    exit 1
fi
grep -Fq "required package path is missing: src/lib.rs" \
    "$test_root/missing-output.txt"

echo "Package file policy tests passed"
