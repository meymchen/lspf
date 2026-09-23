#!/usr/bin/env bash
set -euo pipefail

# Maintainer-approved exception for the Document API in #330. This expires once
# the base workspace version advances from 1.0.2; normal semver checks stay on.
workspace_version() {
    python3 -c 'import sys, tomllib; print(tomllib.loads(sys.stdin.read())["workspace"]["package"]["version"])'
}
if [[ $(workspace_version <Cargo.toml) != 1.0.2 ]]; then
    exit 0
fi
if [[ -z ${RELEASE_PR:-} || $RELEASE_PR == '{}' ]]; then
    exit 0
fi

jq -e '
    (.number | type == "number" and . > 0 and floor == .)
    and .base_branch == "main"
    and (.head_branch | test("^release-plz-[A-Za-z0-9-]+$"))
    and (.releases | length == 1 and .[0].package_name == "lspf")
' <<<"$RELEASE_PR" >/dev/null
number="$(jq -r .number <<<"$RELEASE_PR")"
branch="$(jq -r .head_branch <<<"$RELEASE_PR")"
repository="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"

# Do not edit an unrelated or fork PR even if the Action output is stale.
gh pr view "$number" --repo "$repository" \
    --json state,isCrossRepository,baseRefName,headRefName |
    jq -e --arg branch "$branch" '
        .state == "OPEN" and .isCrossRepository == false
        and .baseRefName == "main" and .headRefName == $branch
    ' >/dev/null
git diff --exit-code
git diff --cached --exit-code
base_revision="$(git rev-parse HEAD)"
gh auth setup-git
git fetch origin refs/heads/main
# A queued workflow from before the release merge must not override a later PR.
if [[ $(git show FETCH_HEAD:Cargo.toml | workspace_version) != 1.0.2 ]]; then
    exit 0
fi
git fetch origin "refs/heads/$branch"
git checkout --detach FETCH_HEAD
git merge-base --is-ancestor "$base_revision" HEAD

# The release-pr Action's 0.3.161 cannot preserve workspace inheritance in
# set-version. Upgrade only this adjustment command, keeping normal release
# generation and its compatibility checks unchanged.
if [[ $(release-plz --version) != 'release-plz 0.3.169' ]]; then
    cargo binstall release-plz@0.3.169 --force \
        --strategies=crate-meta-data,compile --no-confirm
fi
release-plz set-version 1.0.3
bash ci/check-changelog.sh
cargo metadata --locked --no-deps --format-version 1 |
    jq -e '.packages | map(select(.name == "lspf" or .name == "lspf-markdown"))
        | length == 2 and all(.version == "1.0.3")' >/dev/null

# release-plz remains the sole writer of manifests, lockfile and changelog.
# Fail before pushing if it changes anything outside the expected files.
while IFS= read -r file; do
    case "$file" in
        Cargo.toml|Cargo.lock|crates/lspf/CHANGELOG.md) ;;
        *) printf 'unexpected release adjustment: %s\n' "$file" >&2; exit 1 ;;
    esac
done < <(git diff --name-only)

body_file="$(mktemp)"
trap 'rm -f "$body_file"' EXIT
python3 - "$body_file" <<'PY'
from pathlib import Path
import sys

changelog = Path("crates/lspf/CHANGELOG.md").read_text(encoding="utf-8")
entry = "## [1.0.3]" + changelog.split("\n## [1.0.3]", 1)[1].split("\n## [", 1)[0]
body = """## New release

`lspf`: 1.0.2 -> 1.0.3

**One-time versioning exception:** this patch includes incompatible Document API
changes approved in #330. It is not API compatible with 1.0.2.
Use `document.text(None)` for whole reads and `.into_owned()` when an owned
`String` is required. Invalid explicit ranges return the full snapshot; valid
empty ranges return an empty string. Remove the encoding argument from Document
coordinate conversions; snapshots now retain their negotiated encoding.

""" + entry + """

---
Version and changelog generated with [release-plz](https://github.com/release-plz/release-plz/).
The 1.0.3 adjustment is limited to the 1.0.2 base; future releases use normal version selection.
"""
Path(sys.argv[1]).write_text(body, encoding="utf-8")
PY

git add -- Cargo.toml Cargo.lock crates/lspf/CHANGELOG.md
if ! git diff --cached --quiet; then
    git commit -m 'chore: apply approved 1.0.3 release exception'
    git push origin "HEAD:refs/heads/$branch"
fi
gh pr edit "$number" --repo "$repository" \
    --title 'chore: release v1.0.3' --body-file "$body_file"
