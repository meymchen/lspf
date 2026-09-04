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
    .schemaVersion == 2
    and .revision == $revision
    and .release == (.crate + "-" + .version)
    and (.tag | type == "string" and length > 0)
    and (.recordArchive | type == "string" and length > 0)
    and ([.gates[].gate] == ["A", "B", "C", "D", "E"])
    and all(.gates[]; .result == "success")
    and .publishedCrate.matchesCandidate == true
    and .publishedCrate.sha256 == .candidate.sha256
    and (.publishedCrate.downloadUrl | startswith("https://"))
    and (.documentation | startswith("https://"))
    and (.archived.changelogs | type == "array" and length > 0)
    and (.archived.policies | type == "array" and length > 0)
    and (.archived.omitted | type == "array")
    and all(.archived.omitted[];
      (.path | type == "string" and length > 0)
      and (.sha256 | test("^[0-9a-f]{64}$"))
      and (.reason | type == "string" and length > 0)
      and (.url | startswith("https://")))
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
      .archived.changelogs[],
      .archived.policies[],
      .gates[].evidence,
      "release-record.json",
      "release-record.md"
    ][]
  ' "$record")

while IFS= read -r omitted; do
    omitted="${omitted%$'\r'}"
    if [[ $omitted == /* || $omitted == *..* || -e $record_dir/$omitted ]]; then
        printf 'a file declared omitted from the release record is present or unsafe: %s\n' \
            "$omitted" >&2
        exit 1
    fi
done < <(jq -r '.archived.omitted[].path' "$record")

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
)

# The candidate sealed its own hash list before this record chose what to carry,
# so that list still names the omitted artifacts. Check every retained file
# against it, and require each absent one to be declared with exactly the hash
# the candidate sealed -- an omission may drop an artifact, never restate it.
retained="$(mktemp)"
trap 'rm -f "$retained"' EXIT
while read -r sha256 path; do
    path="${path#\*}"
    path="${path#./}"
    path="${path%$'\r'}"
    if [[ -e $record_dir/candidate/$path ]]; then
        printf '%s  %s\n' "$sha256" "$path" >>"$retained"
    elif ! jq -e --arg path "candidate/$path" --arg sha256 "$sha256" '
        any(.archived.omitted[]; .path == $path and .sha256 == $sha256)
      ' "$record" >/dev/null
    then
        printf 'a candidate file is absent from the release record and not declared omitted: %s\n' \
            "$path" >&2
        exit 1
    fi
done <"$record_dir/candidate/SHA256SUMS"

(
    cd "$record_dir/candidate"
    sha256sum --check --strict "$retained"
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
