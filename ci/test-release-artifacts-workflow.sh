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
        "security",
        "test",
        "test-coverage",
        "wasm"
    ] and
    .permissions == {
        "attestations": "write",
        "contents": "write",
        "id-token": "write",
        "pull-requests": "read"
    } and
    ([.steps[] | select((.uses? // "") | startswith("actions/checkout@"))] | length) == 1 and
    ([.steps[] | select((.uses? // "") | startswith("actions/checkout@"))][0].with | has("ref") | not)
' <<<"$release_job" >/dev/null

# The whole artifact pipeline stays ahead of `release-plz release`, so a
# revision that cannot produce provenance never reaches crates.io, and the
# unreleased-revision probe stays ahead of the pipeline it gates.
pending_index="$(jq -r '.steps | map(.name) | index("Detect whether this revision is still unreleased")' \
    <<<"$release_job")"
prepare_index="$(jq -r '.steps | map(.name) | index("Prepare release artifacts from the validated revision")' \
    <<<"$release_job")"
release_index="$(jq -r '.steps | map(.name) | index("Run release-plz")' <<<"$release_job")"
[[ $pending_index != null && $prepare_index != null && $release_index != null ]]
[[ $pending_index -lt $prepare_index && $prepare_index -lt $release_index ]]

# Ordinary pushes to `main` release nothing, so packaging, signing, attesting,
# and artifact retention must all be skipped unless this revision is unreleased.
# Without these guards every commit writes a Sigstore transparency-log entry.
jq -e --arg guard "steps.pending.outputs.pending == 'true'" '
    . as $job
    | [
        "Prepare release artifacts from the validated revision",
        "Generate crate SBOM",
        "Generate build provenance statement",
        "Generate SBOM attestation",
        "Retain attestation statements and artifact hashes",
        "Retain traceable release artifacts"
      ]
    | all(. as $name | any($job.steps[]; .name == $name and .if == $guard))
' <<<"$release_job" >/dev/null

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
        .if == ("steps.pending.outputs.pending == '\''true'\'' && "
            + "steps.release.outputs.releases_created == '\''true'\''") and
        (.run | contains("gh release upload")) and
        (.run | contains("--clobber"))
    )
' <<<"$release_job" >/dev/null

# Publishing stays a human decision: only merging the release pull request may
# reach crates.io. The unreleased-revision probe above spells the tag as
# `v$version`, and `ci/prepare-release-artifacts.sh` derives its release tag the
# same way, so both break silently if `git_tag_name` ever changes shape.
$yq_bin -p toml -o json '.' "$release_plz_config" | jq -e '
    .workspace.release_always == false and
    any(.package[]; .name == "lspf" and .git_tag_name == "v{{ version }}")
' >/dev/null

# release-plz's `semver_check` shells out to `cargo-semver-checks` and silently
# does nothing when the binary is absent, which would let a breaking change ship
# as a minor bump. Pin the install, and pin it to the version the `public-api`
# gate uses so both agree on what counts as breaking.
semver_checks_install='cargo install cargo-semver-checks --version 0.50.0 --locked'
jq -e --arg install "$semver_checks_install" '
    any(.jobs["release-plz-pr"].steps[]; (.run // "") == $install) and
    any(.jobs["public-api"].steps[]; (.run // "") == $install)
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
