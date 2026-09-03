#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

revision="$(git rev-parse HEAD)"
candidate="$test_root/candidate"
evidence="$test_root/evidence"
mkdir -p "$candidate" "$evidence"

for file in \
    lspf-1.0.0.crate \
    CHANGELOG.md \
    lspf-CHANGELOG.md \
    lspf-1.0.0.spdx.json
do
    printf 'fixture for %s\n' "$file" >"$candidate/$file"
done

docs_root="$test_root/docs"
mkdir "$docs_root"
jq -n --arg revision "$revision" '{schemaVersion: 1, revision: $revision}' \
    >"$docs_root/release-docs-metadata.json"
tar -czf "$candidate/lspf-1.0.0-docs.tar.gz" \
    -C "$docs_root" release-docs-metadata.json

jq -n --arg revision "$revision" '{
    schemaVersion: 1,
    crate: "lspf",
    version: "1.0.0",
    revision: $revision,
    tag: "v1.0.0",
    sourceRepository: "https://github.com/meymchen/lspf",
    workflowRun: "https://github.com/meymchen/lspf/actions/runs/123",
    artifacts: {
      crate: "lspf-1.0.0.crate",
      docs: "lspf-1.0.0-docs.tar.gz",
      changelogs: ["CHANGELOG.md", "lspf-CHANGELOG.md"],
      sbom: "lspf-1.0.0.spdx.json",
      hashes: "SHA256SUMS",
      provenance: "provenance.jsonl",
      sbomAttestation: "sbom-attestation.jsonl"
    }
  }' >"$candidate/release-metadata.json"

for gate in A B C D; do
    gate_dir="$evidence/gate-${gate,,}"
    mkdir -p "$gate_dir"
    jq -n --arg gate "$gate" --arg revision "$revision" '{
        schemaVersion: 1,
        gate: $gate,
        revision: $revision,
        workflowRun: "https://github.com/meymchen/lspf/actions/runs/123",
        overallResult: "success",
        failedChecks: (if $gate == "D" then null else [] end),
        failedComponents: (if $gate == "D" then [] else null end),
        humanJudgments: []
      }' >"$gate_dir/evidence.json"
    printf '# Gate %s evidence\n' "$gate" >"$gate_dir/evidence.md"
done

bash ci/prepare-release-candidate.sh "$revision" "$candidate" "$evidence" \
    >/dev/null
printf '{"fixture":"provenance"}\n' >"$candidate/provenance.jsonl"
printf '{"fixture":"sbom-attestation"}\n' \
    >"$candidate/sbom-attestation.jsonl"
(
    cd "$candidate"
    find . -type f ! -name SHA256SUMS -print0 \
        | sort -z \
        | xargs -0 sha256sum \
        >SHA256SUMS
)

# The fixture crate is deliberately not a tar archive, so verification must
# eventually fail. It must first accept the complete hash list, including the
# binary-marker prefix emitted by sha256sum under Git Bash on Windows.
if bash ci/check-release-candidate.sh "$revision" "$candidate" \
    >"$test_root/covered.output" 2>&1
then
    echo 'test failure: the deliberately invalid fixture crate passed' >&2
    exit 1
fi
if grep -F 'not covered by SHA256SUMS' "$test_root/covered.output" >/dev/null; then
    echo 'test failure: a complete SHA256SUMS list was rejected' >&2
    exit 1
fi

printf 'not covered by the retained hashes\n' >"$candidate/unhashed.txt"

if bash ci/check-release-candidate.sh "$revision" "$candidate" \
    >"$test_root/check.output" 2>&1
then
    echo 'test failure: an unhashed candidate file passed verification' >&2
    exit 1
fi
grep -F 'not covered by SHA256SUMS' "$test_root/check.output" >/dev/null

echo 'Unhashed release candidate file rejection verified'
