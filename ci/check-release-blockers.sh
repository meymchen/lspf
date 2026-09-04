#!/usr/bin/env bash
set -euo pipefail

# The register records what validation found, not what automation proved. A
# framework-owned P0 or P1 that is still `open` has no disposition, so it blocks
# the release; an `accepted` one is surfaced as a maintainer judgment instead of
# being silently absorbed into a passing gate.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
register="${1:-$repo_root/ci/release-blockers-v1.json}"
editor_manifest="${2:-$repo_root/editor-validation/journeys-v1.json}"

for file in "$register" "$editor_manifest"; do
    if [[ ! -f $file ]]; then
        printf 'missing release blocker input: %s\n' "$file" >&2
        exit 1
    fi
done

if ! jq -e '
    .schemaVersion == 1
    and (.blockers | type == "array")
    and ([.blockers[].id] | length) == ([.blockers[].id] | unique | length)
    and all(.blockers[];
      (.id | type == "string" and test("^[a-z0-9]+(-[a-z0-9]+)*$"))
      and (.severity | IN("P0", "P1", "P2", "P3"))
      and (.owner | IN("framework", "editor", "toolchain", "downstream"))
      and (.statement | type == "string" and length > 0)
      and (.issue | type == "string"
        and test("^https://github.com/meymchen/lspf/issues/[0-9]+$"))
      and (.disposition | IN("resolved", "accepted", "open"))
      and (.disposition == "resolved"
        or (.justification | type == "string" and length > 0))
    )
  ' "$register" >/dev/null 2>&1
then
    printf 'release blocker register is missing or malformed: %s\n' \
        "$register" >&2
    exit 1
fi

# Every gap the editor journeys tracked must be dispositioned here too, so the
# register cannot stay empty while validation is discovering blockers.
untracked_gaps="$(jq -r \
    --slurpfile register "$register" '
    [$register[0].blockers[].issue] as $registered
    | [.frameworkGaps.tracked[]? | select((.issue | IN($registered[])) | not) | .issue]
    | .[]
  ' "$editor_manifest")"
if [[ -n $untracked_gaps ]]; then
    echo 'editor journeys tracked a framework gap the release blocker register does not disposition:' >&2
    printf '%s\n' "$untracked_gaps" >&2
    exit 1
fi

undisposed="$(jq -r '
    .blockers[]
    | select(.owner == "framework"
      and (.severity | IN("P0", "P1"))
      and .disposition == "open")
    | "- " + .severity + " " + .id + " (" + .issue + "): " + .statement
  ' "$register")"
if [[ -n $undisposed ]]; then
    echo 'undisposed framework-owned P0 or P1 blockers remain:' >&2
    printf '%s\n' "$undisposed" >&2
    exit 1
fi

count="$(jq -r '.blockers | length' "$register")"
accepted="$(jq -r '
    [.blockers[]
      | select(.owner == "framework" and (.severity | IN("P0", "P1")))
      | select(.disposition == "accepted")]
    | length
  ' "$register")"
printf 'Release blocker register verified: %s recorded, %s framework P0/P1 accepted by maintainers\n' \
    "$count" "$accepted"
