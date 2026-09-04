#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source ci/release-candidate-test-helpers.sh

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

revision="$(git rev-parse HEAD)"
candidate="$test_root/candidate"
evidence="$test_root/candidate-evidence"
create_release_candidate_fixture "$revision" "$candidate" "$evidence"
bash ci/prepare-release-candidate.sh "$revision" "$candidate" "$evidence" \
    >/dev/null
printf '{"fixture":"provenance"}\n' >"$candidate/provenance.jsonl"
printf '{"fixture":"sbom-attestation"}\n' >"$candidate/sbom-attestation.jsonl"
(
    cd "$candidate"
    find . -type f ! -name SHA256SUMS -print0 \
        | sort -z \
        | xargs -0 sha256sum \
        >SHA256SUMS
)

candidate_sha256="$(sha256sum "$candidate/lspf-1.0.0.crate" | awk '{print $1}')"

gate_e="$test_root/gate-e"
mkdir -p "$gate_e"
write_gate_e_evidence() {
    jq -n \
        --arg revision "$1" \
        --arg sha256 "$2" \
        --arg result "${3:-success}" '{
        schemaVersion: 1,
        gate: "E",
        revision: $revision,
        sourceRepository: "https://github.com/meymchen/lspf",
        workflowRun: "https://github.com/meymchen/lspf/actions/runs/5150",
        candidate: {
          crate: "lspf",
          version: "1.0.0",
          artifact: "lspf-1.0.0.crate",
          sha256: $sha256,
          graft: "crates/lspf"
        },
        overallResult: $result,
        failedJourneys: (if $result == "success" then [] else
          [{id: "candidate-timeout", name: "Handler and outbound timeout", result: "failure"}]
        end),
        humanJudgments: [{
          classification: "human",
          status: "pending",
          statement: "Editor UI quality remains a human observation."
        }]
      }' >"$4/evidence.json"
    printf '# Gate E candidate validation evidence\n' >"$4/evidence.md"
}
write_gate_e_evidence "$revision" "$candidate_sha256" success "$gate_e"

published="$test_root/published-lspf-1.0.0.crate"
cp "$candidate/lspf-1.0.0.crate" "$published"

record="$test_root/record"
bash ci/prepare-release-record.sh \
    "$revision" "$candidate" "$gate_e" "$published" "$record" >/dev/null

jq -e \
    --arg revision "$revision" \
    --arg sha256 "$candidate_sha256" '
      .schemaVersion == 1
      and .revision == $revision
      and .release == "lspf-1.0.0"
      and .tag == "v1.0.0"
      and ([.gates[].gate] == ["A", "B", "C", "D", "E"])
      and all(.gates[]; .result == "success")
      and .candidate.sha256 == $sha256
      and .publishedCrate.sha256 == $sha256
      and .publishedCrate.matchesCandidate == true
      and .publishedCrate.archive == "published/lspf-1.0.0.crate"
      and .archived.provenance == "candidate/provenance.jsonl"
      and .archived.sbom == "candidate/lspf-1.0.0.spdx.json"
      and .archived.sbomAttestation == "candidate/sbom-attestation.jsonl"
      and .archived.documentation == "candidate/lspf-1.0.0-docs.tar.gz"
      and (.archived.changelogs | sort)
        == ["candidate/CHANGELOG.md", "candidate/lspf-CHANGELOG.md"]
      and (.archived.policies | index("policies/SECURITY.md")) != null
      and (.archived.policies | index("policies/ci/release-blockers-v1.json")) != null
      and (.humanJudgments | length > 0)
    ' "$record/release-record.json" >/dev/null

grep -F 'Matches the validated candidate: **true**' "$record/release-record.md" \
    >/dev/null
grep -F -- '- Gate E: `success`' "$record/release-record.md" >/dev/null

bash ci/check-release-record.sh "$revision" "$record" >/dev/null
echo 'Successful release record preparation and verification confirmed'

# A registry artifact that is not the validated candidate must never be recorded.
divergent="$test_root/divergent.crate"
cp "$candidate/lspf-1.0.0.crate" "$divergent"
printf 'republished by hand\n' >>"$divergent"
if bash ci/prepare-release-record.sh \
    "$revision" "$candidate" "$gate_e" "$divergent" "$test_root/divergent-record" \
    >"$test_root/divergent.output" 2>&1
then
    echo 'test failure: a published crate that differs from the candidate was recorded' >&2
    exit 1
fi
grep -F 'published crate does not match the validated candidate' \
    "$test_root/divergent.output" >/dev/null
if [[ -e $test_root/divergent-record ]]; then
    echo 'test failure: a rejected release record left output behind' >&2
    exit 1
fi
echo 'Divergent published crate rejection verified'

# Gate E must have validated this exact artifact, not merely this revision.
foreign_gate_e="$test_root/foreign-gate-e"
mkdir -p "$foreign_gate_e"
write_gate_e_evidence "$revision" \
    0000000000000000000000000000000000000000000000000000000000000000 \
    success "$foreign_gate_e"
if bash ci/prepare-release-record.sh \
    "$revision" "$candidate" "$foreign_gate_e" "$published" \
    "$test_root/foreign-record" >"$test_root/foreign.output" 2>&1
then
    echo 'test failure: Gate E evidence for another artifact was recorded' >&2
    exit 1
fi
grep -F 'Gate E evidence validated a different candidate artifact' \
    "$test_root/foreign.output" >/dev/null
echo 'Mismatched Gate E artifact rejection verified'

# Failing Gate E evidence cannot be archived as a completed release.
failing_gate_e="$test_root/failing-gate-e"
mkdir -p "$failing_gate_e"
write_gate_e_evidence "$revision" "$candidate_sha256" failure "$failing_gate_e"
if bash ci/prepare-release-record.sh \
    "$revision" "$candidate" "$failing_gate_e" "$published" \
    "$test_root/failing-record" >"$test_root/failing.output" 2>&1
then
    echo 'test failure: failing Gate E evidence produced a release record' >&2
    exit 1
fi
grep -F 'Gate E evidence is missing, malformed, failing, or names another revision' \
    "$test_root/failing.output" >/dev/null
echo 'Failing Gate E rejection verified'

# Tampering after assembly must be caught by the record's own hash list.
printf 'appended after the record was sealed\n' \
    >>"$record/policies/SECURITY.md"
if bash ci/check-release-record.sh "$revision" "$record" \
    >"$test_root/tampered.output" 2>&1
then
    echo 'test failure: a tampered archived policy passed verification' >&2
    exit 1
fi
grep -F 'SECURITY.md: FAILED' "$test_root/tampered.output" >/dev/null

printf 'not covered by the sealed hashes\n' >"$record/stowaway.txt"
if bash ci/check-release-record.sh "$revision" "$record" \
    >"$test_root/stowaway.output" 2>&1
then
    echo 'test failure: an unhashed release record file passed verification' >&2
    exit 1
fi
grep -F 'not covered by SHA256SUMS' "$test_root/stowaway.output" >/dev/null

echo 'Release record tamper detection verified'
