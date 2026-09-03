#!/usr/bin/env bash
set -euo pipefail

revision="${1:?usage: check-release-candidate.sh REVISION CANDIDATE_DIRECTORY}"
candidate_dir="${2:?usage: check-release-candidate.sh REVISION CANDIDATE_DIRECTORY}"
cargo_bin="${CARGO_BIN:-cargo}"

if [[ ! $revision =~ ^[0-9a-f]{40}$ ]]; then
    printf 'release candidate revision must be a full lowercase commit SHA: %s\n' \
        "$revision" >&2
    exit 1
fi

release_metadata="$candidate_dir/release-metadata.json"
candidate_metadata="$candidate_dir/candidate-metadata.json"
if ! jq -e \
    --arg revision "$revision" \
    --slurpfile release "$release_metadata" '
    $release[0] as $release
    | .schemaVersion == 1
      and .revision == $revision
      and .candidate == ($release.crate + "-" + $release.version)
      and .releaseMetadata == "release-metadata.json"
      and .candidateReport == "candidate.md"
      and .artifacts == $release.artifacts
      and ([.gates[].gate] == ["A", "B", "C", "D"])
      and all(.gates[]; .result == "success")
      and (.humanJudgments | type == "array" and length > 0)
  ' "$candidate_metadata" >/dev/null 2>&1
then
    echo 'candidate metadata is missing, malformed, or names another revision' >&2
    exit 1
fi

while IFS= read -r artifact; do
    artifact="${artifact%$'\r'}"
    if [[ $artifact == */* || ! -s $candidate_dir/$artifact ]]; then
        printf 'verified candidate artifact is missing, empty, or unsafe: %s\n' \
            "$artifact" >&2
        exit 1
    fi
done < <(jq -r '
    [
      .artifacts.crate,
      .artifacts.docs,
      .artifacts.changelogs[],
      .artifacts.sbom,
      .artifacts.hashes,
      .artifacts.provenance,
      .artifacts.sbomAttestation,
      .candidateReport,
      "candidate-metadata.json",
      "release-metadata.json"
    ][]
  ' "$candidate_metadata")

for gate in A B C D; do
    gate_file="$candidate_dir/evidence/gate-${gate,,}/evidence.json"
    if ! jq -e --arg gate "$gate" --arg revision "$revision" '
        .gate == $gate
        and .revision == $revision
        and .overallResult == "success"
        and if $gate == "D" then
          (.failedComponents | type == "array" and length == 0)
        else
          (.failedChecks | type == "array" and length == 0)
        end
      ' "$gate_file" >/dev/null 2>&1
    then
        printf 'retained Gate %s evidence does not establish this candidate\n' \
            "$gate" >&2
        exit 1
    fi
done

if ! diff -u \
    <(find "$candidate_dir" -type f ! -name SHA256SUMS \
        -printf '%P\n' | sort) \
    <(awk '{print $2}' "$candidate_dir/SHA256SUMS" \
        | sed -e 's#^\*##' -e 's#^\./##' | sort) \
    >/dev/null
then
    echo 'one or more candidate files are not covered by SHA256SUMS' >&2
    exit 1
fi

(
    cd "$candidate_dir"
    sha256sum --check --strict SHA256SUMS
)

crate_name="$(jq -r '.crate' "$release_metadata")"
crate_name="${crate_name%$'\r'}"
crate_version="$(jq -r '.version' "$release_metadata")"
crate_version="${crate_version%$'\r'}"
crate_file="$(jq -r '.artifacts.crate' "$release_metadata")"
crate_file="${crate_file%$'\r'}"
package_root="$crate_name-$crate_version"
vcs_info="$(
    tar -xOzf "$candidate_dir/$crate_file" \
        "$package_root/.cargo_vcs_info.json"
)"
if ! jq -e --arg revision "$revision" '
    .git.sha1 == $revision and ((.git.dirty // false) == false)
  ' <<<"$vcs_info" >/dev/null
then
    echo 'candidate crate does not identify the validated revision as clean source' >&2
    exit 1
fi

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT
tar -xzf "$candidate_dir/$crate_file" -C "$test_root"

dependency_dir="$test_root/$package_root"
dependency_manifest_dir="$dependency_dir"
if command -v cygpath >/dev/null 2>&1; then
    dependency_manifest_dir="$(cygpath -m "$dependency_dir")"
fi

consumer_dir="$test_root/consumer"
mkdir -p "$consumer_dir/src"
cat >"$consumer_dir/Cargo.toml" <<EOF
[package]
name = "release-candidate-consumer"
version = "0.0.0"
edition = "2024"

[dependencies]
lspf = { path = "$dependency_manifest_dir", default-features = false, features = ["stdio"] }
tokio = { version = "1", features = ["rt-multi-thread"] }
EOF

cat >"$consumer_dir/src/main.rs" <<'EOF'
use lspf::Server;

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the Tokio runtime starts");
    let server = Server::builder(())
        .build()
        .expect("the empty server is valid");
    let outcome = runtime
        .block_on(lspf::stdio(server).serve())
        .expect("the stdio lifecycle completes");
    std::process::exit(outcome.code());
}
EOF

echo 'Compiling a clean external consumer from the retained candidate crate'
"$cargo_bin" generate-lockfile --manifest-path "$consumer_dir/Cargo.toml"
CARGO_TARGET_DIR="$test_root/consumer-target" \
    "$cargo_bin" build --manifest-path "$consumer_dir/Cargo.toml" --locked

frame() {
    local body=$1
    printf 'Content-Length: %d\r\n\r\n%s' "${#body}" "$body"
}

echo 'Running a complete stdio lifecycle from the retained candidate crate'
lifecycle_output=$(
    {
        frame '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}'
        frame '{"jsonrpc":"2.0","method":"initialized","params":{}}'
        frame '{"jsonrpc":"2.0","id":2,"method":"shutdown"}'
        frame '{"jsonrpc":"2.0","method":"exit"}'
    } | "$test_root/consumer-target/debug/release-candidate-consumer"
)

if [[ $lifecycle_output != *'"id":1'* || $lifecycle_output != *'"id":2'* ]]; then
    echo 'the release candidate consumer did not return both lifecycle responses' >&2
    exit 1
fi

echo "Verified release candidate installation for $revision"
