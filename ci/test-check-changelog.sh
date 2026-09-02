#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT

manifest="$fixture_dir/Cargo.toml"
changelog="$fixture_dir/CHANGELOG.md"

write_manifest() {
    cat >"$manifest" <<TOML
[workspace]
members = ["crates/lspf"]

[workspace.package]
version = "$1"
edition = "2024"
TOML
}

write_changelog() {
    cat >"$changelog" <<'MARKDOWN'
# Changelog

## [Unreleased]

## [0.11.0](https://example.invalid/compare/v0.10.0...v0.11.0) - 2026-09-02

### Changed

- [**breaking**] Rename a helper

MARKDOWN
    printf '%s\n' "$@" >>"$changelog"
}

write_manifest 0.11.0
write_changelog '  Explain the rename.'
bash "$repo_root/ci/check-changelog.sh" "$manifest" "$changelog"

write_manifest 0.12.0
if output="$(bash "$repo_root/ci/check-changelog.sh" "$manifest" "$changelog" 2>&1)"; then
    echo 'expected a manifest version with no changelog entry to fail' >&2
    exit 1
fi
grep -F "no entry for the manifest version 0.12.0" <<<"$output" >/dev/null

# `0.1.0` must not satisfy `0.11.0` through an unescaped `.` in the pattern.
write_manifest 0.1.0
if output="$(bash "$repo_root/ci/check-changelog.sh" "$manifest" "$changelog" 2>&1)"; then
    echo 'expected the version match to treat dots literally' >&2
    exit 1
fi
grep -F "no entry for the manifest version 0.1.0" <<<"$output" >/dev/null

write_manifest 0.11.0
write_changelog '  Clear the hand-written `## [Unreleased]` entry as part of the switch.'
if output="$(bash "$repo_root/ci/check-changelog.sh" "$manifest" "$changelog" 2>&1)"; then
    echo 'expected an unreleased heading marker inside prose to fail' >&2
    exit 1
fi
grep -F 'unreleased heading marker inside prose' <<<"$output" >/dev/null

rm -f "$changelog"
if output="$(bash "$repo_root/ci/check-changelog.sh" "$manifest" "$changelog" 2>&1)"; then
    echo 'expected a missing changelog to fail' >&2
    exit 1
fi
grep -F 'file does not exist' <<<"$output" >/dev/null

write_changelog '  Explain the rename.'
printf '%s\n' '[workspace]' >"$manifest"
if output="$(bash "$repo_root/ci/check-changelog.sh" "$manifest" "$changelog" 2>&1)"; then
    echo 'expected a manifest without a workspace version to fail' >&2
    exit 1
fi
grep -F 'cannot read the workspace version' <<<"$output" >/dev/null
