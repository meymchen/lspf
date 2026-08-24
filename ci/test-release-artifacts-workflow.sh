#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workflow="$repo_root/.github/workflows/release-plz.yml"
security_workflow="$repo_root/.github/workflows/security.yml"
permissions_policy="$repo_root/ci/workflow-permissions.json"
yq_bin="${YQ_BIN:-yq}"

workflow_json="$($yq_bin -o=json '.' "$workflow")"
release_job="$(jq -c '.jobs["release-plz-release"]' <<<"$workflow_json")"

jq -e '
    (.needs | sort) == ["packaged-crate", "public-docs", "security"] and
    .permissions == {
        "attestations": "write",
        "contents": "write",
        "id-token": "write",
        "pull-requests": "read"
    } and
    ([.steps[] | select((.uses? // "") | startswith("actions/checkout@"))] | length) == 1 and
    [.steps[] | select((.uses? // "") | startswith("actions/checkout@"))][0].with.ref == "${{ github.sha }}"
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

$yq_bin -e '
    .jobs.supply-chain.steps[] |
    select(.run == "bash ci/test-release-artifacts-workflow.sh")
' "$security_workflow" >/dev/null

jq -e '
    .workflows[".github/workflows/release-plz.yml"]["release-plz-release"] == {
        "attestations": "write",
        "contents": "write",
        "id-token": "write",
        "pull-requests": "read"
    }
' "$permissions_policy" >/dev/null

echo "Release artifact workflow contract verified"
