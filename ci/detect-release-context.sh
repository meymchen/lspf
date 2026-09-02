#!/usr/bin/env bash

set -euo pipefail

repository="${1:?usage: detect-release-context.sh REPOSITORY BASE_BRANCH}"
base_branch="${2:?usage: detect-release-context.sh REPOSITORY BASE_BRANCH}"
output="${GITHUB_OUTPUT:?GITHUB_OUTPUT must name the workflow output file}"

version="${RELEASE_VERSION:-$({
    awk '
        /^\[workspace\.package\]$/ { in_workspace_package = 1; next }
        /^\[/ { in_workspace_package = 0 }
        in_workspace_package && /^version[[:space:]]*=/ {
            value = $0
            sub(/^[^"]*"/, "", value)
            sub(/".*/, "", value)
            print value
            exit
        }
    ' Cargo.toml
})}"

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
    printf 'could not resolve the workspace release version: %s\n' "$version" >&2
    exit 1
fi

if [[ -n ${RELEASE_TAG_EXISTS:-} ]]; then
    if [[ $RELEASE_TAG_EXISTS != true && $RELEASE_TAG_EXISTS != false ]]; then
        printf 'RELEASE_TAG_EXISTS must be true or false, found %s\n' \
            "$RELEASE_TAG_EXISTS" >&2
        exit 2
    fi
    tag_exists="$RELEASE_TAG_EXISTS"
elif git rev-parse -q --verify "refs/tags/v$version^{commit}" >/dev/null; then
    tag_exists=true
else
    tag_exists=false
fi

if [[ $tag_exists == true ]]; then
    {
        printf 'pending=false\n'
        printf 'authorized=false\n'
        printf 'pull-request=\n'
        printf 'version=%s\n' "$version"
    } >>"$output"
    printf 'Tag v%s already exists; skipping release validation\n' "$version"
    exit 0
fi

{
    printf 'pending=true\n'
    printf 'version=%s\n' "$version"
} >>"$output"

bash ci/authorize-release.sh "$version" "$repository" "$base_branch"
