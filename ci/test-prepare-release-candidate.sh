#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source ci/release-candidate-test-helpers.sh

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

revision="$(git rev-parse HEAD)"
artifacts="$test_root/release-artifacts"
evidence="$test_root/evidence"
create_release_candidate_fixture "$revision" "$artifacts" "$evidence"

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
docs_root="$test_root/wrong-docs"
mkdir "$docs_root"
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

# The candidate carries whatever version the authorized release-plz pull request
# bumped to, so no version is privileged. What must not pass is a bundle whose
# parts disagree: metadata naming one version over a documentation archive built
# for another.
wrong_version_artifacts="$test_root/wrong-version-artifacts"
wrong_version_evidence="$test_root/wrong-version-evidence"
mkdir "$wrong_version_artifacts"
cp \
    "$artifacts/lspf-1.0.0.crate" \
    "$artifacts/lspf-1.0.0-docs.tar.gz" \
    "$artifacts/CHANGELOG.md" \
    "$artifacts/lspf-CHANGELOG.md" \
    "$artifacts/lspf-1.0.0.spdx.json" \
    "$artifacts/release-metadata.json" \
    "$wrong_version_artifacts"
cp -R "$evidence" "$wrong_version_evidence"
jq '.version = "1.1.0"' "$wrong_version_artifacts/release-metadata.json" \
    >"$wrong_version_artifacts/release-metadata.next"
mv "$wrong_version_artifacts/release-metadata.next" \
    "$wrong_version_artifacts/release-metadata.json"

if bash ci/prepare-release-candidate.sh \
    "$revision" "$wrong_version_artifacts" "$wrong_version_evidence" \
    >"$test_root/wrong-version.output" 2>&1
then
    echo 'test failure: mismatched artifact versions produced a candidate' >&2
    exit 1
fi
grep -F 'documentation archive' "$test_root/wrong-version.output" >/dev/null

echo 'Mismatched candidate version rejection verified'

# A release that is not 1.0.0 is an ordinary release, not an error.
next_artifacts="$test_root/next-artifacts"
next_evidence="$test_root/next-evidence"
create_release_candidate_fixture "$revision" "$next_artifacts" "$next_evidence" \
    1.1.0

bash ci/prepare-release-candidate.sh "$revision" "$next_artifacts" "$next_evidence"

jq -e '
    .candidate == "lspf-1.1.0"
    and .artifacts.crate == "lspf-1.1.0.crate"
    and .artifacts.docs == "lspf-1.1.0-docs.tar.gz"
  ' "$next_artifacts/candidate-metadata.json" >/dev/null

echo 'Post-1.0 candidate preparation verified'
