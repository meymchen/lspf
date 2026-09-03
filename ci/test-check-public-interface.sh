#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
checker="$repo_root/ci/check-public-interface.sh"
inventory=docs/public-interface.md

bash "$checker" "$repo_root" >/dev/null

work_root="$(mktemp -d)"
trap 'rm -rf "$work_root"' EXIT

# Each case runs the checker against a private copy of the tracked inputs, so a
# mutation proves the gate reacts rather than leaving the repository altered.
case_root=""
new_case() {
    case_root="$work_root/$1"
    rm -rf "$case_root"
    mkdir -p "$case_root/ci" "$case_root/docs" \
        "$case_root/crates/lspf/src" "$case_root/crates/lspf/tests"
    cp "$repo_root/ci/check-public-interface.sh" "$case_root/ci/"
    cp "$repo_root/$inventory" "$case_root/docs/"
    cp "$repo_root/SECURITY.md" "$case_root/"
    cp "$repo_root/crates/lspf/Cargo.toml" "$case_root/crates/lspf/"
    cp "$repo_root/crates/lspf/src/lib.rs" \
        "$repo_root/crates/lspf/src/testing.rs" \
        "$repo_root/crates/lspf/src/features.rs" \
        "$case_root/crates/lspf/src/"
    cp "$repo_root/crates/lspf/tests/catalog.rs" "$case_root/crates/lspf/tests/"
}

expect_rejected() {
    local explanation=$1
    local expected=$2
    local output

    if output=$(bash "$case_root/ci/check-public-interface.sh" "$case_root" 2>&1); then
        printf 'checker accepted %s\n' "$explanation" >&2
        printf '%s\n' "$output" >&2
        exit 1
    fi
    if [[ $output != *"$expected"* ]]; then
        printf 'checker rejected %s without the expected diagnostic: %s\n' \
            "$explanation" "$expected" >&2
        printf '%s\n' "$output" >&2
        exit 1
    fi
}

expect_accepted() {
    local explanation=$1
    local output

    if ! output=$(bash "$case_root/ci/check-public-interface.sh" "$case_root" 2>&1); then
        printf 'checker rejected %s\n' "$explanation" >&2
        printf '%s\n' "$output" >&2
        exit 1
    fi
}

new_case unmodified
expect_accepted "an unmodified copy of the tracked inputs"

new_case new-export
cat >>"$case_root/crates/lspf/src/lib.rs" <<'EOF'

pub use workspace::Workspace as WorkspaceAlias;
EOF
expect_rejected "a crate-root export missing from the inventory" \
    "crate root exports 'WorkspaceAlias', which docs/public-interface.md does not freeze"

new_case direct-root-type
cat >>"$case_root/crates/lspf/src/lib.rs" <<'EOF'

pub struct AccidentalRootType;
EOF
expect_rejected "a direct crate-root type missing from the inventory" \
    "crate root exports 'AccidentalRootType', which docs/public-interface.md does not freeze"

new_case removed-export
grep -v '^| `ProgressOptions` |' "$repo_root/$inventory" \
    >"$case_root/docs/public-interface.md"
expect_rejected "an inventory that dropped a live export" \
    "crate root exports 'ProgressOptions', which docs/public-interface.md does not freeze"

new_case stale-inventory-row
sed 's/^| `ProgressOptions` | any |/| `ProgressPolicy` | any |/' \
    "$repo_root/$inventory" >"$case_root/docs/public-interface.md"
expect_rejected "an inventory row with no matching export" \
    "docs/public-interface.md freezes 'ProgressPolicy', which the crate root does not export"

new_case wrong-availability
sed 's/^| `OsFileProvider` | native-runtime |/| `OsFileProvider` | any |/' \
    "$repo_root/$inventory" >"$case_root/docs/public-interface.md"
expect_rejected "an inventory row with the wrong availability" \
    "crate root export 'OsFileProvider' is available under 'native-runtime', frozen as 'any'"

new_case unknown-availability
sed 's/^#\[cfg(target_arch = "wasm32")\]$/#[cfg(feature = "invented")]/' \
    "$repo_root/crates/lspf/src/lib.rs" >"$case_root/crates/lspf/src/lib.rs"
expect_rejected "an export behind an undocumented availability gate" \
    "uses an availability gate docs/public-interface.md does not define"

new_case frozen-but-hidden
sed 's/^| `ProgressOptions` | any |/| `fuzzing` | fuzzing |/' \
    "$repo_root/$inventory" >"$case_root/docs/public-interface.md"
expect_rejected "an inventory that freezes a doc-hidden export" \
    "crate root export 'fuzzing' is hidden from documentation, so it cannot be frozen"

new_case new-testing-item
cat >>"$case_root/crates/lspf/src/testing.rs" <<'EOF'

/// An undocumented addition to the testing surface.
pub struct ScriptedClock;
EOF
expect_rejected "a testing export missing from the inventory" \
    "lspf::testing exports 'ScriptedClock', which docs/public-interface.md does not freeze"

new_case new-testing-reexport
cat >>"$case_root/crates/lspf/src/testing.rs" <<'EOF'

pub use crate::RawMessage as CapturedMessage;
EOF
expect_rejected "a testing re-export missing from the inventory" \
    "lspf::testing exports 'CapturedMessage', which docs/public-interface.md does not freeze"

new_case new-type-alias
sed 's/^        WorkspaceOptions as WorkspaceServerCapabilities,$/        WorkspaceOptions as WorkspaceServerCapabilities, Uri as DocumentUri,/' \
    "$repo_root/crates/lspf/src/lib.rs" >"$case_root/crates/lspf/src/lib.rs"
expect_rejected "a types alias missing from the inventory" \
    "lspf::types aliases 'DocumentUri', which docs/public-interface.md does not freeze"

new_case accidental-types-declaration
sed '/^pub mod types {$/a\    pub struct AccidentalProtocolType;' \
    "$repo_root/crates/lspf/src/lib.rs" >"$case_root/crates/lspf/src/lib.rs"
expect_rejected "an unsupported public declaration in lspf::types" \
    "lspf::types contains unsupported public declaration:     pub struct AccidentalProtocolType;"

new_case accidental-request-reexport
sed '/^    pub mod request {$/a\        pub use crate::RawMessage as InternalRequestMessage;' \
    "$repo_root/crates/lspf/src/lib.rs" >"$case_root/crates/lspf/src/lib.rs"
expect_rejected "an unsupported public re-export in lspf::types::request" \
    "lspf::types contains unsupported public declaration:         pub use crate::RawMessage as InternalRequestMessage;"

new_case unexercised-descriptor
cat >>"$case_root/crates/lspf/src/features.rs" <<'EOF'

/// An undocumented catalog addition.
pub fn telepathy() -> HoverFeature {
    hover()
}
EOF
expect_rejected "a feature descriptor no catalog journey registers" \
    "feature descriptor 'features::telepathy' is not registered by crates/lspf/tests/catalog.rs"

new_case accidental-feature-type
cat >>"$case_root/crates/lspf/src/features.rs" <<'EOF'

/// An accidental public implementation type.
pub struct TelepathyFeature;
EOF
expect_rejected "a public feature type that no descriptor returns" \
    "lspf::features exports type 'TelepathyFeature', which no catalog descriptor returns"

new_case accidental-feature-reexport
cat >>"$case_root/crates/lspf/src/features.rs" <<'EOF'

pub use crate::RawMessage as FeatureInternals;
EOF
expect_rejected "an unsupported public feature re-export" \
    "lspf::features contains unsupported public declaration: pub use crate::RawMessage as FeatureInternals;"

new_case undocumented-cargo-feature
sed 's/^fuzzing = \[/telemetry-otlp = ["runtime-tokio"]\nfuzzing = [/' \
    "$repo_root/crates/lspf/Cargo.toml" >"$case_root/crates/lspf/Cargo.toml"
expect_rejected "a Cargo feature the support contract never describes" \
    "Cargo feature 'telemetry-otlp' is not described by SECURITY.md"

echo "public interface freeze contract tests passed"
