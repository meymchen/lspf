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
    ci/policy/public-api-breaking-approvals.json
    ci/policy/workflow-permissions.json
    ci/policy/npm-allowed-licenses.txt
    ci/policy/release-blockers-v1.json
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
docs_file="$(jq -r '.artifacts.docs' "$release_metadata")"
docs_file="${docs_file%$'\r'}"
download_url="${RELEASE_REGISTRY_DOWNLOAD_URL:-$registry_url/api/v1/crates/$crate_name/$crate_version/download}"
docs_url="${RELEASE_DOCS_URL:-https://docs.rs/$crate_name/$crate_version}"
record_archive="$crate_name-$crate_version-release-record.tar.gz"
tag_url="$server_url/$repository/blob/$release_tag"
archive_url="$server_url/$repository/releases/download/$release_tag/$record_archive"

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

# The rendered documentation is the one candidate artifact this record does not
# carry: docs.rs builds and hosts it from the published crate, so archiving it
# would store a second copy of something the registry already keeps. Dropping it
# leaves `candidate/SHA256SUMS` naming a file that is no longer here, so the
# omission is recorded with the hash that list sealed and verified against it.
if [[ ! -s $record/candidate/$docs_file ]]; then
    printf 'candidate documentation is missing or empty: %s\n' "$docs_file" >&2
    exit 1
fi
docs_sha256="$(sha256sum "$record/candidate/$docs_file" | awk '{print $1}')"
rm "$record/candidate/$docs_file"
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
    --arg docs_file "$docs_file" \
    --arg docs_sha256 "$docs_sha256" \
    --arg docs_url "$docs_url" \
    --arg record_archive "$record_archive" \
    --argjson policies "$policy_paths" \
    --slurpfile release "$release_metadata" \
    --slurpfile candidate "$candidate_dir/candidate-metadata.json" \
    --slurpfile gate_e "$record/evidence/gate-e/evidence.json" '
    $release[0] as $release
    | $candidate[0] as $candidate
    | {
        schemaVersion: 2,
        revision: $revision,
        release: ($crate + "-" + $version),
        crate: $crate,
        version: $version,
        tag: $tag,
        sourceRepository: ($server + "/" + $repository),
        workflowRun: $workflow_run,
        recordArchive: $record_archive,
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
          changelogs: [$release.artifacts.changelogs[] | "candidate/" + .],
          policies: $policies,
          omitted: [{
            path: ("candidate/" + $docs_file),
            sha256: $docs_sha256,
            reason: "The rendered documentation is built and hosted from the published crate.",
            url: $docs_url
          }]
        },
        documentation: $docs_url,
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

# This rendering is both the file inside the archive and the body of the GitHub
# release. A release body resolves a relative link against the repository, where
# none of these paths exist, so every link here is absolute and every path
# *inside* the archive is written as code rather than as a link.
jq -r '
  def blob: .sourceRepository + "/blob/" + .tag + "/";
  . as $record
  | "# Release record for " + .release + "\n",
  "Revision: [`" + .revision[0:7] + "`](" + .sourceRepository + "/commit/" + .revision + ")",
  "Tag: [`" + .tag + "`](" + .sourceRepository + "/tree/" + .tag + ")",
  "Published: [" + .release + "](" + .publishedCrate.registry + "/crates/"
    + .crate + "/" + .version + ") on ["
    + (.publishedCrate.registry | sub("^https?://"; "")) + "]("
    + .publishedCrate.registry + ") ([.crate download]("
    + .publishedCrate.downloadUrl + "))",
  "Published crate hash: `sha256:" + .publishedCrate.sha256 + "`",
  "Matches the validated candidate: **"
    + (.publishedCrate.matchesCandidate | tostring) + "**",
  "Documentation: " + .documentation + "\n",
  "## The archive\n",
  "Everything named below lives in [" + .recordArchive + "]("
    + .sourceRepository + "/releases/download/" + .tag + "/" + .recordArchive
    + "), not in this repository.\n",
  "```",
  "tar -xzf " + .recordArchive + " -C release-record",
  "cd release-record && sha256sum --check SHA256SUMS",
  "```\n",
  "`SHA256SUMS` covers every file in the archive. Its external anchors are the crate hash above, which "
    + (.publishedCrate.registry | sub("^https?://"; ""))
    + " serves independently, and the workflow run below.\n",
  "## Gate evidence\n",
  "All gates succeeded in [workflow run " + (.workflowRun | split("/") | last)
    + "](" + .workflowRun + ").\n",
  "| Gate | Result | Path in the archive |",
  "| --- | --- | --- |",
  (.gates[] |
    "| " + .gate + " | `" + .result + "` | `" + .evidence + "` |"),
  "\n## Archived artifacts\n",
  "| Artifact | Path in the archive |",
  "| --- | --- |",
  "| Candidate report | `" + .candidate.report + "` |",
  "| Candidate hashes | `" + .candidate.hashes + "` |",
  "| Candidate crate | `" + .candidate.archive + "` |",
  "| Published crate | `" + .publishedCrate.archive + "` |",
  "| Provenance | `" + .archived.provenance + "` |",
  "| SBOM | `" + .archived.sbom + "` |",
  "| SBOM attestation | `" + .archived.sbomAttestation + "` |",
  (.archived.changelogs[] | "| Changelog | `" + . + "` |"),
  "",
  "`" + .candidate.archive + "` and `" + .publishedCrate.archive
    + "` are byte-identical, and both match the hash the registry serves. That equality is the claim this record exists to support, so both copies are kept: it stays checkable from the archive alone.",
  "\n### Not archived\n",
  (.archived.omitted[] |
    "- `" + .path + "` — " + .reason + " `sha256:" + .sha256 + "`, still named by `candidate/SHA256SUMS`. Read it at " + .url + "."),
  "\n## Archived policies\n",
  "The policies in force at the released revision. The archive stores them under `policies/`; each copy is byte-identical to the file at the tag, linked here for reading without downloading the archive.\n",
  (.archived.policies[] |
    . as $p | ($p | sub("^policies/"; "")) as $path
    | "- [" + $path + "](" + ($record | blob) + $path + ")"),
  "\n## Human judgments\n",
  "These items are deliberately not presented as automated proof.\n",
  (.humanJudgments[] |
    "- " + .statement + " Status: "
    + (if (.status // "") == "" then "not recorded" else "`" + .status + "`" end)
    + ".")
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
