#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

crate_name=lspf
revision="${1:?usage: prepare-release-artifacts.sh REVISION OUTPUT_DIRECTORY}"
output_dir="${2:?usage: prepare-release-artifacts.sh REVISION OUTPUT_DIRECTORY}"

if [[ -e $output_dir ]]; then
    printf 'release artifact output already exists: %s\n' "$output_dir" >&2
    exit 1
fi

revision="$(git rev-parse --verify "${revision}^{commit}")"
head_revision="$(git rev-parse --verify HEAD)"

if [[ $head_revision != "$revision" ]]; then
    printf 'validated revision %s does not match checked-out revision %s\n' \
        "$revision" "$head_revision" >&2
    exit 1
fi

if ! git diff --quiet "$revision" --; then
    echo 'tracked source differs from the validated revision' >&2
    exit 1
fi

# The workflow publishes the currently validated revision after proving that a
# matching release pull request was merged into its history. Refuse merge
# commits because their packaged tree is not represented by one reviewable
# parent revision, which would weaken the source-to-provenance link.
parent_count=$(($(git rev-list --parents -n 1 "$revision" | wc -w) - 1))
if ((parent_count > 1)); then
    printf 'release revision %s is a merge commit with %d parents; merge release pull requests with squash or rebase\n' \
        "$revision" "$parent_count" >&2
    exit 1
fi

crate_version="$(cargo metadata --no-deps --format-version 1 \
    | jq -er --arg name "$crate_name" \
        '.packages[] | select(.name == $name) | .version')"
release_tag="v$crate_version"
crate_file="$crate_name-$crate_version.crate"

echo "Packaging $crate_name $crate_version from $revision"
cargo package -p "$crate_name" --locked

package_path="target/package/$crate_file"
package_root="$crate_name-$crate_version"
vcs_info="$(tar -xOzf "$package_path" "$package_root/.cargo_vcs_info.json")"

if ! jq -e --arg revision "$revision" \
    '.git.sha1 == $revision and ((.git.dirty // false) == false)' \
    <<<"$vcs_info" >/dev/null; then
    echo 'packaged crate does not identify the validated revision as clean source' >&2
    exit 1
fi

mkdir -p "$output_dir"
cp "$package_path" "$output_dir/$crate_file"
cp CHANGELOG.md "$output_dir/CHANGELOG.md"
cp crates/lspf/CHANGELOG.md "$output_dir/lspf-CHANGELOG.md"

metadata_path="$output_dir/release-metadata.json"
sbom_path="$output_dir/$crate_name-$crate_version.spdx.json"

jq -n \
    --arg crate "$crate_name" \
    --arg version "$crate_version" \
    --arg revision "$revision" \
    --arg tag "$release_tag" \
    --arg repository "https://github.com/${GITHUB_REPOSITORY:-meymchen/lspf}" \
    --arg workflow_run "${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-meymchen/lspf}/actions/runs/${GITHUB_RUN_ID:-local}" \
    --arg crate_file "$crate_file" \
    '{
        schemaVersion: 1,
        crate: $crate,
        version: $version,
        revision: $revision,
        tag: $tag,
        sourceRepository: $repository,
        workflowRun: $workflow_run,
        artifacts: {
            crate: $crate_file,
            changelogs: ["CHANGELOG.md", "lspf-CHANGELOG.md"],
            sbom: ($crate + "-" + $version + ".spdx.json"),
            hashes: "SHA256SUMS",
            provenance: "provenance.jsonl",
            sbomAttestation: "sbom-attestation.jsonl"
        }
    }' >"$metadata_path"

if [[ -n ${GITHUB_OUTPUT:-} ]]; then
    {
        printf 'directory=%s\n' "$output_dir"
        printf 'crate=%s\n' "$output_dir/$crate_file"
        printf 'metadata=%s\n' "$metadata_path"
        printf 'sbom=%s\n' "$sbom_path"
        printf 'tag=%s\n' "$release_tag"
        printf 'version=%s\n' "$crate_version"
    } >>"$GITHUB_OUTPUT"
fi

echo "Prepared release metadata for $release_tag at revision $revision"
