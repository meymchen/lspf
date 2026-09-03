#!/usr/bin/env bash
set -euo pipefail

# The release record is what remains after the release: one archive that ties
# the crate on the registry back to the candidate that Gates A through E
# validated, together with the provenance, SBOM, policies, and changelog that
# were in force at that revision.

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source ci/release-candidate-helpers.sh

usage='usage: prepare-release-record.sh REVISION CANDIDATE_DIRECTORY GATE_E_DIRECTORY PUBLISHED_CRATE OUTPUT_DIRECTORY'
revision="${1:?$usage}"
candidate_dir="${2:?$usage}"
gate_e_dir="${3:?$usage}"
published_crate="${4:?$usage}"
output_dir="${5:?$usage}"
registry_url="${RELEASE_REGISTRY_URL:-https://crates.io}"
repository="${GITHUB_REPOSITORY:-meymchen/lspf}"
server_url="${GITHUB_SERVER_URL:-https://github.com}"
workflow_run="${RELEASE_RECORD_WORKFLOW_RUN:-$server_url/$repository/actions/runs/${GITHUB_RUN_ID:-local}}"

policies=(
    SECURITY.md
    deny.toml
    docs/public-interface.md
    ci/public-api-breaking-approvals.json
    ci/workflow-permissions.json
    ci/npm-allowed-licenses.txt
    ci/release-blockers-v1.json
)

if [[ ! $revision =~ ^[0-9a-f]{40}$ ]]; then
    printf 'release record revision must be a full lowercase commit SHA: %s\n' \
        "$revision" >&2
    exit 1
fi
if [[ -e $output_dir ]]; then
    printf 'release record output already exists: %s\n' "$output_dir" >&2
    exit 1
fi
if [[ ! -s $published_crate ]]; then
    printf 'published crate is missing or empty: %s\n' "$published_crate" >&2
    exit 1
fi

release_metadata="$candidate_dir/release-metadata.json"
validate_candidate_metadata "$revision" "$candidate_dir/candidate-metadata.json"
validate_release_gate_evidence "$revision" "$candidate_dir/evidence"

read_release_crate_identity "$release_metadata"
release_tag="$(jq -r '.tag' "$release_metadata")"
release_tag="${release_tag%$'\r'}"
download_url="${RELEASE_REGISTRY_DOWNLOAD_URL:-$registry_url/api/v1/crates/$crate_name/$crate_version/download}"

# Everything is assembled in a staging directory and moved into place only
# once, so a rejected release never leaves a partial record behind.
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
record="$staging/record"

mkdir -p "$record/evidence"
cp -R "$gate_e_dir" "$record/evidence/gate-e"
validate_release_gate_evidence "$revision" "$record/evidence" E

candidate_sha256="$(sha256sum "$candidate_dir/$crate_file" | awk '{print $1}')"
published_sha256="$(sha256sum "$published_crate" | awk '{print $1}')"

# Gate E validated one artifact. Refuse to archive its evidence against a
# different candidate crate.
if ! jq -e --arg sha256 "$candidate_sha256" --arg artifact "$crate_file" '
    .candidate.sha256 == $sha256 and .candidate.artifact == $artifact
  ' "$record/evidence/gate-e/evidence.json" >/dev/null
then
    echo 'Gate E evidence validated a different candidate artifact' >&2
    exit 1
fi

if [[ $published_sha256 != "$candidate_sha256" ]]; then
    printf 'published crate does not match the validated candidate\n  candidate: %s\n  published: %s\n' \
        "$candidate_sha256" "$published_sha256" >&2
    exit 1
fi

cp -R "$candidate_dir" "$record/candidate"
mkdir -p "$record/published"
cp "$published_crate" "$record/published/$crate_file"
for policy in "${policies[@]}"; do
    if [[ ! -s $policy ]]; then
        printf 'release policy is missing or empty: %s\n' "$policy" >&2
        exit 1
    fi
    mkdir -p "$record/policies/$(dirname "$policy")"
    cp "$policy" "$record/policies/$policy"
done

policy_paths="$(printf 'policies/%s\n' "${policies[@]}" \
    | jq -Rsc 'split("\n") | map(select(length > 0))')"

jq -n \
    --arg revision "$revision" \
    --arg crate "$crate_name" \
    --arg version "$crate_version" \
    --arg tag "$release_tag" \
    --arg crate_file "$crate_file" \
    --arg candidate_sha256 "$candidate_sha256" \
    --arg published_sha256 "$published_sha256" \
    --arg registry "$registry_url" \
    --arg download "$download_url" \
    --arg repository "$repository" \
    --arg server "$server_url" \
    --arg workflow_run "$workflow_run" \
    --argjson policies "$policy_paths" \
    --slurpfile release "$release_metadata" \
    --slurpfile candidate "$candidate_dir/candidate-metadata.json" \
    --slurpfile gate_e "$record/evidence/gate-e/evidence.json" '
    $release[0] as $release
    | $candidate[0] as $candidate
    | {
        schemaVersion: 1,
        revision: $revision,
        release: ($crate + "-" + $version),
        crate: $crate,
        version: $version,
        tag: $tag,
        sourceRepository: ($server + "/" + $repository),
        workflowRun: $workflow_run,
        candidate: {
          metadata: "candidate/candidate-metadata.json",
          report: "candidate/candidate.md",
          hashes: "candidate/SHA256SUMS",
          archive: ("candidate/" + $crate_file),
          sha256: $candidate_sha256
        },
        publishedCrate: {
          registry: $registry,
          downloadUrl: $download,
          archive: ("published/" + $crate_file),
          sha256: $published_sha256,
          matchesCandidate: ($published_sha256 == $candidate_sha256)
        },
        archived: {
          provenance: ("candidate/" + $release.artifacts.provenance),
          sbom: ("candidate/" + $release.artifacts.sbom),
          sbomAttestation: ("candidate/" + $release.artifacts.sbomAttestation),
          documentation: ("candidate/" + $release.artifacts.docs),
          changelogs: [$release.artifacts.changelogs[] | "candidate/" + .],
          policies: $policies
        },
        gates: (
          [$candidate.gates[] | {
            gate,
            result,
            workflowRun,
            evidence: ("candidate/" + .evidence)
          }]
          + [{
            gate: $gate_e[0].gate,
            result: $gate_e[0].overallResult,
            workflowRun: $gate_e[0].workflowRun,
            evidence: "evidence/gate-e/evidence.json"
          }]
        ),
        humanJudgments: (
          [$candidate.humanJudgments[]?]
          + [$gate_e[0].humanJudgments[]?]
          + [{
              classification: "human",
              status: "recorded",
              statement: "A maintainer authorized this publication; the archive proves only that the published crate is the validated candidate."
            }]
          | unique_by(.statement)
        )
      }
  ' >"$record/release-record.json"

jq -r '
  "# Release record for " + .release + "\n",
  "Revision: [" + .revision + "](" + .sourceRepository + "/commit/" + .revision + ")",
  "Tag: `" + .tag + "`",
  "Published: [" + .release + "](" + .publishedCrate.downloadUrl + ") on "
    + .publishedCrate.registry,
  "Published crate hash: `sha256:" + .publishedCrate.sha256 + "`",
  "Matches the validated candidate: **"
    + (.publishedCrate.matchesCandidate | tostring) + "**\n",
  "## Gate evidence\n",
  (.gates[] |
    "- Gate " + .gate + ": `" + .result + "` ([evidence](" + .evidence + "))"),
  "\n## Archived artifacts\n",
  "- Candidate report: [candidate.md](" + .candidate.report + ")",
  "- Candidate hashes: [SHA256SUMS](" + .candidate.hashes + ")",
  "- Provenance: [" + .archived.provenance + "](" + .archived.provenance + ")",
  "- SBOM: [" + .archived.sbom + "](" + .archived.sbom + ")",
  "- SBOM attestation: [" + .archived.sbomAttestation + "](" + .archived.sbomAttestation + ")",
  "- Documentation: [" + .archived.documentation + "](" + .archived.documentation + ")",
  (.archived.changelogs[] | "- Changelog: [" + . + "](" + . + ")"),
  "\n## Archived policies\n",
  (.archived.policies[] | "- [" + . + "](" + . + ")"),
  "\n## Human judgments\n",
  "These items are deliberately not presented as automated proof.\n",
  (.humanJudgments[] | "- " + .statement + " Status: `" + .status + "`.")
' "$record/release-record.json" >"$record/release-record.md"

(
    cd "$record"
    # Only the record's own list is excluded; the candidate's nested list is
    # hashed like any other archived file.
    find . -type f ! -path ./SHA256SUMS -print0 \
        | sort -z \
        | xargs -0 sha256sum \
        >SHA256SUMS
)

mkdir -p "$(dirname "$output_dir")"
mv "$record" "$output_dir"

printf 'Prepared the release record for %s-%s at %s\n' \
    "$crate_name" "$crate_version" "$revision"
