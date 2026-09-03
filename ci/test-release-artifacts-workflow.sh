#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workflow="$repo_root/.github/workflows/ci.yml"
security_workflow="$repo_root/.github/workflows/security.yml"
permissions_policy="$repo_root/ci/workflow-permissions.json"
release_plz_config="$repo_root/release-plz.toml"
yq_bin="${YQ_BIN:-yq}"

workflow_json="$($yq_bin -o=json '.' "$workflow")"
release_job="$(jq -c '.jobs["release-plz-release"]' <<<"$workflow_json")"
release_context="$(jq -c '.jobs["release-context"]' <<<"$workflow_json")"

# The release job must depend on the same checks Gate A reports on, so a
# revision cannot reach crates.io without clearing the gates that declare it
# releasable. `ci/test-gate-a-evidence-workflow.sh` pins the other half of this
# pair; the two lists are meant to stay identical.
jq -e '
    (.needs | sort) == [
        "feature-contract",
        "markdownlint",
        "msrv",
        "native-lifecycle",
        "native-matrix",
        "packaged-crate",
        "performance-baseline",
        "public-api",
        "public-docs",
        "release-context",
        "security",
        "test",
        "test-coverage",
        "wasm"
    ] and
    .if == "${{ github.event_name == '\''push'\'' && needs.release-context.outputs.authorized == '\''true'\'' }}" and
    .permissions == {
        "attestations": "write",
        "contents": "write",
        "id-token": "write",
        "pull-requests": "read"
    } and
    ([.steps[] | select((.uses? // "") | startswith("actions/checkout@"))] | length) == 1 and
    ([.steps[] | select((.uses? // "") | startswith("actions/checkout@"))][0].with | has("ref") | not)
' <<<"$release_job" >/dev/null

# Release classification happens before the expensive gate fan-out. The
# release job itself is reachable only for a pending version authorized by a
# merged release-plz PR, so every artifact step can remain unconditional and a
# failed run remains retryable until the tag exists.
jq -e '
    .outputs.authorized == "${{ steps.release.outputs.authorized }}" and
    .outputs.pending == "${{ steps.release.outputs.pending }}" and
    any(.steps[];
        .id == "release" and
        (.run | contains("bash ci/detect-release-context.sh"))
    )
' <<<"$release_context" >/dev/null

jq -e '
    . as $job
    | [
        "Prepare retry-safe release-plz config",
        "Prepare release artifacts from the validated revision",
        "Generate crate SBOM",
        "Generate build provenance statement",
        "Generate SBOM attestation",
        "Retain attestation statements and artifact hashes",
        "Retain traceable release artifacts",
        "Run release-plz"
      ]
    | all(. as $name | any($job.steps[]; .name == $name and (has("if") | not)))
' <<<"$release_job" >/dev/null

prepare_index="$(jq -r '.steps | map(.name) | index("Prepare release artifacts from the validated revision")' \
    <<<"$release_job")"
release_index="$(jq -r '.steps | map(.name) | index("Run release-plz")' <<<"$release_job")"
[[ $prepare_index != null && $release_index != null && $prepare_index -lt $release_index ]]

jq -e '
    any(.steps[];
        .name == "Generate crate SBOM" and
        ((.uses? // "") | startswith("anchore/sbom-action@")) and
        .with.file == "${{ steps.artifacts.outputs.crate }}" and
        .with["output-file"] == "${{ steps.artifacts.outputs.sbom }}" and
        .with["upload-artifact"] == false and
        .with["upload-release-assets"] == false
    ) and
    any(.steps[];
        .id == "provenance" and
        ((.uses? // "") | startswith("actions/attest@")) and
        (.with["subject-path"] | contains("${{ steps.artifacts.outputs.crate }}")) and
        (.with["subject-path"] | contains("${{ steps.artifacts.outputs.metadata }}"))
    ) and
    any(.steps[];
        .id == "sbom-attestation" and
        ((.uses? // "") | startswith("actions/attest@")) and
        .with["subject-path"] == "${{ steps.artifacts.outputs.crate }}" and
        .with["sbom-path"] == "${{ steps.artifacts.outputs.sbom }}"
    )
' <<<"$release_job" >/dev/null

jq -e '
    any(.steps[];
        .name == "Retain traceable release artifacts" and
        ((.uses? // "") | startswith("actions/upload-artifact@")) and
        .with.path == "${{ steps.artifacts.outputs.directory }}" and
        .with["if-no-files-found"] == "error" and
        .with["retention-days"] == 90
    ) and
    any(.steps[];
        .name == "Attach artifacts to the GitHub release" and
        .if == "steps.release.outputs.releases_created == '\''true'\''" and
        (.run | contains("gh release upload")) and
        (.run | contains("--clobber"))
    )
' <<<"$release_job" >/dev/null

# Publishing stays a human decision: only merging the release pull request may
# reach crates.io. `ci/detect-release-context.sh` spells the pending tag as
# `v$version`, and `ci/prepare-release-artifacts.sh` derives its release tag the
# same way, so both break silently if `git_tag_name` ever changes shape.
$yq_bin -p toml -o json '.' "$release_plz_config" | jq -e '
    .workspace.release_always == false and
    any(.package[]; .name == "lspf" and .git_tag_name == "v{{ version }}")
' >/dev/null

# Exercise the durable authorization seam with local fixtures. The matching
# release-plz PR authorizes a retry only when its merge commit is in HEAD's
# history; a similarly titled PR from another branch does not.
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT
head_revision="$(git rev-parse HEAD)"
jq -n --arg revision "$head_revision" '[{
    number: 270,
    merged_at: "2026-09-01T06:01:30Z",
    title: "chore: release v0.10.0",
    merge_commit_sha: $revision,
    head: {
        ref: "release-plz-2026-08-31T09-08-13Z",
        repo: {full_name: "meymchen/lspf"}
    }
}]' >"$fixture_dir/authorized.json"

RELEASE_PRS_FILE="$fixture_dir/authorized.json" \
GITHUB_OUTPUT="$fixture_dir/authorized.output" \
    bash "$repo_root/ci/authorize-release.sh" 0.10.0 meymchen/lspf main >/dev/null
grep -Fx 'authorized=true' "$fixture_dir/authorized.output" >/dev/null
grep -Fx 'pull-request=270' "$fixture_dir/authorized.output" >/dev/null

jq 'map(.head.ref = "feature/not-a-release")' \
    "$fixture_dir/authorized.json" >"$fixture_dir/unauthorized.json"
RELEASE_PRS_FILE="$fixture_dir/unauthorized.json" \
GITHUB_OUTPUT="$fixture_dir/unauthorized.output" \
    bash "$repo_root/ci/authorize-release.sh" 0.10.0 meymchen/lspf main >/dev/null
grep -Fx 'authorized=false' "$fixture_dir/unauthorized.output" >/dev/null

# release-plz's `semver_check` shells out to `cargo-semver-checks` and silently
# does nothing when the binary is absent, which would let a breaking change ship
# as a minor bump. Pin the release binaries in the shared setup action instead of
# compiling them from source, and keep both semver consumers on the same version.
semver_checks_tool='cargo-semver-checks@0.50.0'
jq -e \
    --arg semver "$semver_checks_tool" \
    --arg wasm_bindgen 'wasm-bindgen-cli@0.2.126' \
    --arg llvm_cov 'cargo-llvm-cov@0.6.21' '
    any(.jobs["release-plz-pr"].steps[];
        .uses == "./.github/actions/setup-rust" and .with.tools == $semver
    ) and
    any(.jobs["public-api"].steps[];
        .uses == "./.github/actions/setup-rust" and .with.tools == $semver
    ) and
    any(.jobs.wasm.steps[];
        .uses == "./.github/actions/setup-rust" and .with.tools == $wasm_bindgen
    ) and
    any(.jobs["test-coverage"].steps[];
        .uses == "./.github/actions/setup-rust" and .with.tools == $llvm_cov
    ) and
    all(.jobs[].steps[]?;
        ((.run // "") | test("^cargo install (cargo-semver-checks|wasm-bindgen-cli|cargo-llvm-cov)")) | not
    )
' <<<"$workflow_json" >/dev/null

$yq_bin -e '
    .jobs.supply-chain.steps[] |
    select(.run == "bash ci/test-release-artifacts-workflow.sh")
' "$security_workflow" >/dev/null

jq -e '
    .workflows[".github/workflows/ci.yml"]["release-plz-release"] == {
        "attestations": "write",
        "contents": "write",
        "id-token": "write",
        "pull-requests": "read"
    }
' "$permissions_policy" >/dev/null

echo "Release artifact workflow contract verified"
