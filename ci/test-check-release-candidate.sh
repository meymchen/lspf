#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source ci/release-candidate-test-helpers.sh

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

revision="$(git rev-parse HEAD)"
candidate="$test_root/candidate"
evidence="$test_root/evidence"
create_release_candidate_fixture "$revision" "$candidate" "$evidence"

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
