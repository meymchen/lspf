#!/usr/bin/env bash
set -euo pipefail

revision="${1:?usage: prepare-release-candidate.sh REVISION RELEASE_ARTIFACT_DIRECTORY EVIDENCE_DIRECTORY}"
artifact_dir="${2:?usage: prepare-release-candidate.sh REVISION RELEASE_ARTIFACT_DIRECTORY EVIDENCE_DIRECTORY}"
evidence_dir="${3:?usage: prepare-release-candidate.sh REVISION RELEASE_ARTIFACT_DIRECTORY EVIDENCE_DIRECTORY}"

if [[ ! $revision =~ ^[0-9a-f]{40}$ ]]; then
    printf 'release candidate revision must be a full lowercase commit SHA: %s\n' \
        "$revision" >&2
    exit 1
fi

release_metadata="$artifact_dir/release-metadata.json"
if ! jq -e --arg revision "$revision" '
    .schemaVersion == 1
    and .revision == $revision
    and .crate == "lspf"
    and (.version | type == "string" and length > 0)
    and (.artifacts | type == "object")
  ' "$release_metadata" >/dev/null 2>&1
then
    echo 'release metadata is missing, malformed, or names another revision' >&2
    exit 1
fi

while IFS= read -r artifact; do
    artifact="${artifact%$'\r'}"
    if [[ $artifact == */* || ! -f $artifact_dir/$artifact ]]; then
        printf 'release candidate artifact is missing or unsafe: %s\n' \
            "$artifact" >&2
        exit 1
    fi
done < <(jq -r '
    [
      .artifacts.crate,
      .artifacts.docs,
      .artifacts.changelogs[],
      .artifacts.sbom
    ][]
  ' "$release_metadata")

docs_artifact="$(jq -r '.artifacts.docs' "$release_metadata")"
docs_artifact="${docs_artifact%$'\r'}"
docs_metadata=''
if ! docs_metadata="$(
    tar -xOzf "$artifact_dir/$docs_artifact" release-docs-metadata.json \
        2>/dev/null \
    || tar -xOzf "$artifact_dir/$docs_artifact" ./release-docs-metadata.json \
        2>/dev/null
)" \
    || ! jq -e --arg revision "$revision" \
        '.schemaVersion == 1 and .revision == $revision' \
        <<<"$docs_metadata" >/dev/null 2>&1
then
    echo 'documentation archive is missing revision-matching metadata' >&2
    exit 1
fi

for gate in A B C D; do
    gate_file="$evidence_dir/gate-${gate,,}/evidence.json"
    if ! jq -e --arg gate "$gate" --arg revision "$revision" '
        .schemaVersion == 1
        and .gate == $gate
        and .revision == $revision
        and .overallResult == "success"
        and if $gate == "D" then
          (.failedComponents | type == "array" and length == 0)
        else
          (.failedChecks | type == "array" and length == 0)
        end
      ' "$gate_file" >/dev/null 2>&1
    then
        printf 'Gate %s evidence is missing, malformed, failing, or names another revision\n' \
            "$gate" >&2
        exit 1
    fi
done

if [[ -e $artifact_dir/evidence || -e $artifact_dir/candidate-metadata.json ]]; then
    echo 'release candidate output already exists' >&2
    exit 1
fi

mkdir "$artifact_dir/evidence"
for gate in a b c d; do
    cp -R "$evidence_dir/gate-$gate" "$artifact_dir/evidence/gate-$gate"
done

jq -n \
    --slurpfile release "$release_metadata" \
    --slurpfile gate_a "$evidence_dir/gate-a/evidence.json" \
    --slurpfile gate_b "$evidence_dir/gate-b/evidence.json" \
    --slurpfile gate_c "$evidence_dir/gate-c/evidence.json" \
    --slurpfile gate_d "$evidence_dir/gate-d/evidence.json" '
    $release[0] as $release
    | [$gate_a[0], $gate_b[0], $gate_c[0], $gate_d[0]] as $gates
    | {
        schemaVersion: 1,
        candidate: ($release.crate + "-" + $release.version),
        revision: $release.revision,
        sourceRepository: $release.sourceRepository,
        workflowRun: $release.workflowRun,
        releaseMetadata: "release-metadata.json",
        candidateReport: "candidate.md",
        artifacts: $release.artifacts,
        gates: [
          $gates[] | {
            gate,
            result: .overallResult,
            workflowRun,
            evidence: ("evidence/gate-" + (.gate | ascii_downcase) + "/evidence.json")
          }
        ],
        humanJudgments: (
          [$gates[].humanJudgments[]?]
          + [{
              classification: "human",
              status: "pending",
              statement: "A maintainer must confirm that no framework-owned P0 or P1 blocker remains before the candidate can be published."
            }, {
              classification: "human",
              status: "pending",
              statement: "Publishing the exact candidate remains a separate maintainer authorization."
            }]
          | unique_by(.statement)
        )
      }
  ' >"$artifact_dir/candidate-metadata.json"

jq -r '
    "# Verified release candidate\n",
    "Revision: " + .revision,
    "Candidate: `" + .candidate + "`\n",
    "## Automated gates\n",
    (.gates[] |
      "- Gate " + .gate + ": `" + .result + "` ([evidence](" + .evidence + "))"),
    "\n## Human judgments\n",
    "These items are deliberately not presented as automated proof.\n",
    (.humanJudgments[] | "- " + .statement)
  ' "$artifact_dir/candidate-metadata.json" >"$artifact_dir/candidate.md"

echo "Prepared release candidate for $revision"
