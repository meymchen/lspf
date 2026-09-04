#!/usr/bin/env bash
set -euo pipefail

# `cargo publish` packages the checked-out source rather than uploading an
# artifact, so the crate it sends is only the validated candidate if packaging
# this revision reproduces those bytes. Run this before publishing: afterwards
# a mismatch is a permanently published crate no evidence covers.

cd "$(dirname "${BASH_SOURCE[0]}")/.."

usage='usage: check-repackaged-candidate.sh REVISION CANDIDATE_CRATE'
revision="${1:?$usage}"
candidate_crate="${2:?$usage}"
cargo_bin="${CARGO_BIN:-cargo}"

if [[ ! $revision =~ ^[0-9a-f]{40}$ ]]; then
    printf 'repackaging revision must be a full lowercase commit SHA: %s\n' \
        "$revision" >&2
    exit 1
fi
if [[ ! -s $candidate_crate ]]; then
    printf 'candidate crate is missing or empty: %s\n' "$candidate_crate" >&2
    exit 1
fi

head_revision="$(git rev-parse --verify HEAD)"
if [[ $head_revision != "$revision" ]]; then
    printf 'validated revision %s does not match checked-out revision %s\n' \
        "$revision" "$head_revision" >&2
    exit 1
fi

echo "Repackaging $revision to compare with the validated candidate"
"$cargo_bin" package -p lspf --locked

crate_file="$(basename "$candidate_crate")"
repackaged="target/package/$crate_file"
if [[ ! -s $repackaged ]]; then
    printf 'repackaging did not produce %s\n' "$repackaged" >&2
    exit 1
fi

candidate_sha256="$(sha256sum "$candidate_crate" | awk '{print $1}')"
repackaged_sha256="$(sha256sum "$repackaged" | awk '{print $1}')"

if [[ $candidate_sha256 != "$repackaged_sha256" ]]; then
    printf 'packaging this revision no longer reproduces the validated candidate; refusing to publish\n  candidate:   %s\n  repackaged:  %s\n' \
        "$candidate_sha256" "$repackaged_sha256" >&2
    exit 1
fi

printf 'Repackaging %s reproduces the validated candidate (sha256:%s)\n' \
    "$revision" "$candidate_sha256"
