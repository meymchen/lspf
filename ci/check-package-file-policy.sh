#!/usr/bin/env bash

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 PACKAGE_LIST" >&2
    exit 2
fi

package_list=$1
if [[ ! -s $package_list ]]; then
    echo "package file list is empty: $package_list" >&2
    exit 1
fi

declare -A present=()

while IFS= read -r path; do
    present["$path"]=1

    case "$path" in
        target | target/* | */target | */target/* | \
            .git | .git/* | */.git | */.git/* | \
            .github | .github/* | */.github | */.github/* | \
            .scratch | .scratch/* | */.scratch | */.scratch/* | \
            .env | .env.* | */.env | */.env.* | \
            *.key | *.pem)
            echo "forbidden package path: $path" >&2
            exit 1
            ;;
    esac

    case "$path" in
        .cargo_vcs_info.json | Cargo.lock | Cargo.toml | Cargo.toml.orig | \
            README.md | CHANGELOG.md | LICENSE | LICENSE-* | build.rs | \
            src/* | examples/* | tests/* | benches/*)
            ;;
        *)
            echo "unexpected package path: $path" >&2
            exit 1
            ;;
    esac
done <"$package_list"

for required in \
    .cargo_vcs_info.json \
    CHANGELOG.md \
    Cargo.lock \
    Cargo.toml \
    Cargo.toml.orig \
    README.md \
    src/lib.rs; do
    if [[ ! -v present["$required"] ]]; then
        echo "required package path is missing: $required" >&2
        exit 1
    fi
done

echo "Package file policy verified"
