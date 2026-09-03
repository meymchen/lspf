#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

revision="$(git rev-parse HEAD)"
artifacts="$test_root/release-artifacts"
evidence="$test_root/evidence"
mkdir -p "$artifacts" "$evidence"

for file in \
    lspf-1.0.0.crate \
    CHANGELOG.md \
    lspf-CHANGELOG.md \
    lspf-1.0.0.spdx.json
do
    printf 'fixture for %s\n' "$file" >"$artifacts/$file"
done

docs_root="$test_root/docs"
mkdir "$docs_root"
jq -n --arg revision "$revision" '{schemaVersion: 1, revision: $revision}' \
    >"$docs_root/release-docs-metadata.json"
tar -czf "$artifacts/lspf-1.0.0-docs.tar.gz" \
    -C "$docs_root" release-docs-metadata.json

jq -n \
    --arg revision "$revision" '
    {
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
    }
  ' >"$artifacts/release-metadata.json"

for gate in A B C D; do
    gate_dir="$evidence/gate-${gate,,}"
    mkdir -p "$gate_dir"
    jq -n \
        --arg gate "$gate" \
        --arg revision "$revision" '
        {
          schemaVersion: 1,
          gate: $gate,
          revision: $revision,
          workflowRun: "https://github.com/meymchen/lspf/actions/runs/123",
          overallResult: "success",
          failedChecks: (if $gate == "D" then null else [] end),
          failedComponents: (if $gate == "D" then [] else null end),
          humanJudgments: (
            if $gate == "A" then [{
              classification: "human",
              statement: "Maintainers decide whether the support promise is acceptable."
            }] else [] end
          )
        }
      ' >"$gate_dir/evidence.json"
    printf '# Gate %s evidence\n' "$gate" >"$gate_dir/evidence.md"
done

bash ci/prepare-release-candidate.sh "$revision" "$artifacts" "$evidence"

jq -e \
    --arg revision "$revision" '
    .schemaVersion == 1
    and .candidate == "lspf-1.0.0"
    and .revision == $revision
    and .releaseMetadata == "release-metadata.json"
    and .candidateReport == "candidate.md"
    and .artifacts.crate == "lspf-1.0.0.crate"
    and .artifacts.docs == "lspf-1.0.0-docs.tar.gz"
    and ([.gates[].gate] == ["A", "B", "C", "D"])
    and all(.gates[]; .result == "success")
    and ([.humanJudgments[].statement] | any(contains("support promise")))
    and ([.humanJudgments[].statement] | any(contains("P0 or P1")))
    and ([.humanJudgments[].statement] | any(contains("Publishing")))
  ' "$artifacts/candidate-metadata.json" >/dev/null
grep -F "Revision: $revision" "$artifacts/candidate.md" >/dev/null
grep -F 'Gate A: `success`' "$artifacts/candidate.md" >/dev/null
grep -F 'Gate D: `success`' "$artifacts/candidate.md" >/dev/null
grep -F '## Human judgments' "$artifacts/candidate.md" >/dev/null
grep -F 'P0 or P1' "$artifacts/candidate.md" >/dev/null

for gate in a b c d; do
    cmp "$evidence/gate-$gate/evidence.json" \
        "$artifacts/evidence/gate-$gate/evidence.json"
    cmp "$evidence/gate-$gate/evidence.md" \
        "$artifacts/evidence/gate-$gate/evidence.md"
done

echo 'Successful release candidate preparation verified'

contradictory_artifacts="$test_root/contradictory-artifacts"
contradictory_evidence="$test_root/contradictory-evidence"
mkdir "$contradictory_artifacts"
cp \
    "$artifacts/lspf-1.0.0.crate" \
    "$artifacts/lspf-1.0.0-docs.tar.gz" \
    "$artifacts/CHANGELOG.md" \
    "$artifacts/lspf-CHANGELOG.md" \
    "$artifacts/lspf-1.0.0.spdx.json" \
    "$artifacts/release-metadata.json" \
    "$contradictory_artifacts"
cp -R "$evidence" "$contradictory_evidence"
jq '.failedChecks = [{id: "security", result: "failure"}]' \
    "$contradictory_evidence/gate-a/evidence.json" \
    >"$contradictory_evidence/gate-a/evidence.next"
mv "$contradictory_evidence/gate-a/evidence.next" \
    "$contradictory_evidence/gate-a/evidence.json"

if bash ci/prepare-release-candidate.sh \
    "$revision" "$contradictory_artifacts" "$contradictory_evidence" \
    >"$test_root/contradictory.output" 2>&1
then
    echo 'test failure: contradictory Gate A evidence produced a candidate' >&2
    exit 1
fi
grep -F 'Gate A evidence' "$test_root/contradictory.output" >/dev/null

echo 'Contradictory gate evidence rejection verified'

wrong_docs_artifacts="$test_root/wrong-docs-artifacts"
wrong_docs_evidence="$test_root/wrong-docs-evidence"
mkdir "$wrong_docs_artifacts"
cp \
    "$artifacts/lspf-1.0.0.crate" \
    "$artifacts/CHANGELOG.md" \
    "$artifacts/lspf-CHANGELOG.md" \
    "$artifacts/lspf-1.0.0.spdx.json" \
    "$artifacts/release-metadata.json" \
    "$wrong_docs_artifacts"
cp -R "$evidence" "$wrong_docs_evidence"
jq -n '{schemaVersion: 1, revision: "0000000000000000000000000000000000000000"}' \
    >"$docs_root/release-docs-metadata.json"
tar -czf "$wrong_docs_artifacts/lspf-1.0.0-docs.tar.gz" \
    -C "$docs_root" release-docs-metadata.json

if bash ci/prepare-release-candidate.sh \
    "$revision" "$wrong_docs_artifacts" "$wrong_docs_evidence" \
    >"$test_root/wrong-docs.output" 2>&1
then
    echo 'test failure: docs from another revision produced a candidate' >&2
    exit 1
fi
grep -F 'documentation archive' "$test_root/wrong-docs.output" >/dev/null

echo 'Mismatched documentation revision rejection verified'
