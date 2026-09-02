#!/usr/bin/env bash
set -euo pipefail

# markdownlint skips `crates/lspf/CHANGELOG.md` because release-plz generates it,
# so the two structural properties a generated changelog still has to hold are
# checked here instead.
manifest="${1:-Cargo.toml}"
changelog="${2:-crates/lspf/CHANGELOG.md}"
status=0

for file in "$manifest" "$changelog"; do
    if [[ ! -f $file ]]; then
        printf 'file does not exist: %s\n' "$file" >&2
        exit 2
    fi
done

version="$(awk '
    /^\[/ { in_table = ($0 == "[workspace.package]"); next }
    in_table && /^version[[:space:]]*=/ {
        sub(/^version[[:space:]]*=[[:space:]]*"/, "")
        sub(/".*$/, "")
        print
        exit
    }
' "$manifest")"

if [[ -z $version ]]; then
    printf 'cannot read the workspace version from %s\n' "$manifest" >&2
    exit 2
fi

# A release publishes whatever the manifest says, and its notes are read out of
# the changelog by version. A missing entry means the two disagree about what is
# being released, which is what a broken changelog generation looks like.
if ! grep -qE "^## \[$(sed 's/\./\\./g' <<<"$version")\]" "$changelog"; then
    printf '%s: no entry for the manifest version %s\n' "$changelog" "$version" >&2
    status=1
fi

# release-plz locates the unreleased heading with a regex that is anchored to the
# start of the document rather than the start of a line, and keeps the last
# match. Any of these spellings inside prose therefore becomes the heading it
# splits on, and the next release lands in the middle of an older entry.
offenders="$(grep -n -F \
    -e '## Unreleased' \
    -e '## [Unreleased]' \
    -e '## unreleased' \
    -e '## [unreleased]' \
    "$changelog" |
    grep -vE '^[0-9]+:## (\[?[Uu]nreleased\]?)[[:space:]]*$' || true)"

if [[ -n $offenders ]]; then
    while IFS= read -r offender; do
        printf '%s:%s: unreleased heading marker inside prose; release-plz splits the changelog here\n' \
            "$changelog" "${offender%%:*}" >&2
    done <<<"$offenders"
    status=1
fi

exit "$status"
