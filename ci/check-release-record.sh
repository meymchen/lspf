#!/usr/bin/env bash
set -euo pipefail

# Verify an assembled release record without trusting the job that produced it:
# every named artifact is present, both hash lists cover and match their files,
# Gates A through E name this revision and passed, and the crate published to
# the registry is byte-identical to the validated candidate.

source "$(dirname "${BASH_SOURCE[0]}")/release-candidate-helpers.sh"

usage='usage: check-release-record.sh REVISION RECORD_DIRECTORY'
revision="${1:?$usage}"
record_dir="${2:?$usage}"

if [[ ! $revision =~ ^[0-9a-f]{40}$ ]]; then
    printf 'release record revision must be a full lowercase commit SHA: %s\n' \
        "$revision" >&2
    exit 1
fi

record="$record_dir/release-record.json"
if ! jq -e --arg revision "$revision" '
    .schemaVersion == 1
    and .revision == $revision
    and .release == (.crate + "-" + .version)
    and (.tag | type == "string" and length > 0)
    and ([.gates[].gate] == ["A", "B", "C", "D", "E"])
    and all(.gates[]; .result == "success")
    and .publishedCrate.matchesCandidate == true
    and .publishedCrate.sha256 == .candidate.sha256
    and (.publishedCrate.downloadUrl | startswith("https://"))
    and (.archived.changelogs | type == "array" and length > 0)
    and (.archived.policies | type == "array" and length > 0)
    and (.humanJudgments | type == "array" and length > 0)
  ' "$record" >/dev/null 2>&1
then
    echo 'release record metadata is missing, malformed, failing, or names another revision' >&2
    exit 1
fi

while IFS= read -r archived; do
    archived="${archived%$'\r'}"
    if [[ $archived == /* || $archived == *..* || ! -s $record_dir/$archived ]]; then
        printf 'archived release record file is missing, empty, or unsafe: %s\n' \
            "$archived" >&2
        exit 1
    fi
done < <(jq -r '
    [
      .candidate.metadata,
      .candidate.report,
      .candidate.hashes,
      .candidate.archive,
      .publishedCrate.archive,
      .archived.provenance,
      .archived.sbom,
      .archived.sbomAttestation,
      .archived.documentation,
      .archived.changelogs[],
      .archived.policies[],
      .gates[].evidence,
      "release-record.json",
      "release-record.md"
    ][]
  ' "$record")

if ! diff -u \
    <(find "$record_dir" -type f -printf '%P\n' | grep -vx SHA256SUMS | sort) \
    <(awk '{print $2}' "$record_dir/SHA256SUMS" \
        | sed -e 's#^\*##' -e 's#^\./##' | sort) \
    >/dev/null
then
    echo 'one or more release record files are not covered by SHA256SUMS' >&2
    exit 1
fi

(
    cd "$record_dir"
    sha256sum --check --strict SHA256SUMS
    cd candidate
    sha256sum --check --strict SHA256SUMS
)

validate_release_gate_evidence "$revision" "$record_dir/candidate/evidence"
validate_release_gate_evidence "$revision" "$record_dir/evidence" E

candidate_archive="$(jq -r '.candidate.archive' "$record")"
published_archive="$(jq -r '.publishedCrate.archive' "$record")"
recorded_sha256="$(jq -r '.publishedCrate.sha256' "$record")"
candidate_archive="${candidate_archive%$'\r'}"
published_archive="${published_archive%$'\r'}"
recorded_sha256="${recorded_sha256%$'\r'}"

candidate_sha256="$(sha256sum "$record_dir/$candidate_archive" | awk '{print $1}')"
published_sha256="$(sha256sum "$record_dir/$published_archive" | awk '{print $1}')"

if [[ $candidate_sha256 != "$recorded_sha256" \
    || $published_sha256 != "$recorded_sha256" ]]; then
    printf 'the archived crates do not match the recorded release hash\n  recorded:  %s\n  candidate: %s\n  published: %s\n' \
        "$recorded_sha256" "$candidate_sha256" "$published_sha256" >&2
    exit 1
fi

if ! jq -e --arg sha256 "$recorded_sha256" '
    .candidate.sha256 == $sha256
  ' "$record_dir/evidence/gate-e/evidence.json" >/dev/null
then
    echo 'Gate E evidence validated a different candidate artifact' >&2
    exit 1
fi

printf 'Verified the release record for %s at %s\n' \
    "$(jq -r '.release' "$record")" "$revision"
