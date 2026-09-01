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
authorization_index="$(jq -r '.steps | map(.name) | index("Authorize the pending release")' \
    <<<"$release_job")"
prepare_index="$(jq -r '.steps | map(.name) | index("Prepare release artifacts from the validated revision")' \
    <<<"$release_job")"
release_index="$(jq -r '.steps | map(.name) | index("Run release-plz")' <<<"$release_job")"
[[ $pending_index != null && $authorization_index != null && $prepare_index != null && $release_index != null ]]
[[ $pending_index -lt $authorization_index && $authorization_index -lt $prepare_index && $prepare_index -lt $release_index ]]

# A failed release run must remain retryable without turning every manifest
# version bump into an implicit publish. A merged release-plz PR is the durable
# authorization signal; the release action is reached only while that signal
# exists in HEAD's history and the matching tag is still absent.
release_guard="steps.pending.outputs.pending == 'true' && steps.authorization.outputs.authorized == 'true'"
jq -e --arg guard "$release_guard" '
    any(.steps[];
        .name == "Authorize the pending release" and
        .id == "authorization" and
        .if == "steps.pending.outputs.pending == '\''true'\''" and
        (.run | contains("bash ci/authorize-release.sh"))
    ) and
    any(.steps[];
        .name == "Prepare retry-safe release-plz config" and
        .id == "release-config" and
        .if == $guard and
        (.run | contains("release_always = true"))
    ) and
    any(.steps[];
        .name == "Run release-plz" and
        .if == $guard and
        .with.config == "${{ runner.temp }}/release-plz.toml"
    )
' <<<"$release_job" >/dev/null

# Ordinary pushes to `main` release nothing, so packaging, signing, attesting,
# and artifact retention must all be skipped unless this version is unreleased
# and a merged release PR authorizes it. Without these guards every commit
# writes a Sigstore transparency-log entry.
jq -e --arg guard "$release_guard" '
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
            + "steps.authorization.outputs.authorized == '\''true'\'' && "
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
