#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../../.."
repo_root="$PWD"
test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT
mkdir -p "$test_root/bin" "$test_root/work/ci" "$test_root/work/crates/lspf"
export TEST_ROOT="$test_root"
export PATH="$test_root/bin:$PATH"
export GITHUB_REPOSITORY=meymchen/lspf
export RELEASE_PR='{"number":328,"base_branch":"main","head_branch":"release-plz-test","releases":[{"package_name":"lspf","version":"2.0.0"}]}'
export PR_STATE=OPEN PR_CROSS=false PR_BRANCH=release-plz-test

cat >"$test_root/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$TEST_ROOT/gh.log"
case "$1 $2" in
    'pr view')
        jq -n --arg state "$PR_STATE" --argjson cross "$PR_CROSS" --arg branch "$PR_BRANCH" \
            '{state:$state,isCrossRepository:$cross,baseRefName:"main",headRefName:$branch}' ;;
    'auth setup-git') ;;
    'pr edit')
        while [[ $1 != --body-file ]]; do shift; done
        cp "$2" "$TEST_ROOT/body.md" ;;
    *) exit 2 ;;
esac
SH
cat >"$test_root/bin/release-plz" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ $1 == --version ]]; then
    if [[ -f $TEST_ROOT/installed ]]; then echo 'release-plz 0.3.169'; else echo 'release-plz 0.3.161'; fi
    exit 0
fi
[[ $* == 'set-version 1.0.3' ]]
echo "$*" >>"$TEST_ROOT/plz.log"
[[ ${FAIL_SET_VERSION:-false} == false ]]
python3 - <<'PY'
from pathlib import Path
for name in ('Cargo.toml', 'Cargo.lock', 'crates/lspf/CHANGELOG.md'):
    p = Path(name)
    p.write_text(p.read_text().replace('2.0.0', '1.0.3'))
PY
SH
cat >"$test_root/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
    binstall)
        [[ $* == 'binstall release-plz@0.3.169 --force --strategies=crate-meta-data,compile --no-confirm' ]]
        touch "$TEST_ROOT/installed" ;;
    metadata)
        [[ $* == 'metadata --locked --no-deps --format-version 1' ]]
        python3 - <<'PY'
import json, tomllib
with open('Cargo.lock', 'rb') as f:
    print(json.dumps({'packages': tomllib.load(f)['package']}))
PY
        ;;
    *) exit 2 ;;
esac
SH
chmod +x "$test_root/bin/gh" "$test_root/bin/release-plz" "$test_root/bin/cargo"
cp ci/check-changelog.sh "$test_root/work/ci/"
cd "$test_root/work"
cat >Cargo.toml <<'TOML'
[workspace.package]
version = "1.0.2"
TOML
cat >Cargo.lock <<'TOML'
[[package]]
name = "lspf"
version = "1.0.2"
[[package]]
name = "lspf-markdown"
version = "1.0.2"
TOML
printf '# Changelog\n\n## [1.0.2]\n\nOld release.\n' >crates/lspf/CHANGELOG.md
git init -q -b main
git config user.name 'Release test'
git config user.email 'release-test@example.invalid'
git config commit.gpgsign false
git config core.autocrlf false
git add .
git commit -qm 'chore: fixture base'
git init -q --bare "$test_root/remote.git"
git remote add origin "$test_root/remote.git"
git push -q origin main
git checkout -qb release-plz-test
python3 - <<'PY'
from pathlib import Path
for name in ('Cargo.toml', 'Cargo.lock'):
    p = Path(name)
    p.write_text(p.read_text().replace('1.0.2', '2.0.0'))
p = Path('crates/lspf/CHANGELOG.md')
s = p.read_text().replace('## [1.0.2]', '## [2.0.0]\n\n- [**breaking**] Document queries changed.\n\n## [1.0.2]')
p.write_text(s)
PY
git add .
git commit -qm 'chore: release v2.0.0'
git push -q origin release-plz-test
original_head="$(git rev-parse HEAD)"
git checkout -q main

run_adjustment() {
    bash "$repo_root/ci/prepare-release-pr.sh" >"$test_root/run.log" 2>&1
}
expect_rejection() {
    if run_adjustment; then
        echo 'expected the release adjustment to be rejected' >&2
        exit 1
    fi
    [[ $(git --git-dir="$test_root/remote.git" rev-parse release-plz-test) == "$original_head" ]]
}

# No output, malformed output, unrelated PRs, and tool failure cannot push.
RELEASE_PR='' run_adjustment
RELEASE_PR='{}' run_adjustment
[[ ! -f $test_root/gh.log ]]
RELEASE_PR='{"number":"328; echo unsafe"}' expect_rejection
PR_CROSS=true expect_rejection
PR_STATE=MERGED expect_rejection
PR_BRANCH=feature-test expect_rejection
FAIL_SET_VERSION=true expect_rejection
[[ ! -f $test_root/body.md ]]
git checkout -q main

# A real local Git remote proves the three generated files are committed and
# pushed to the release branch, without modifying main or requiring credentials.
run_adjustment
release_head="$(git --git-dir="$test_root/remote.git" rev-parse release-plz-test)"
[[ $release_head != "$original_head" ]]
[[ $(git log -1 --format=%s) == 'chore: apply approved 1.0.3 release exception' ]]
[[ $(git diff --name-only "$original_head" HEAD | wc -l) == 3 ]]
grep -F 'not API compatible with 1.0.2' "$test_root/body.md" >/dev/null
grep -F 'document.text(None)' "$test_root/body.md" >/dev/null
grep -F '[**breaking**] Document queries changed.' "$test_root/body.md" >/dev/null
if grep -F 'Old release.' "$test_root/body.md"; then exit 1; fi
grep -F -- '--title chore: release v1.0.3 --body-file' "$test_root/gh.log" >/dev/null
[[ -f $test_root/installed ]]

# Retry repairs PR metadata without producing another commit.
git checkout -q main
run_adjustment
[[ $(git --git-dir="$test_root/remote.git" rev-parse release-plz-test) == "$release_head" ]]

# A queued job from 1.0.2 sees main has advanced and must leave the PR untouched.
git checkout -q main
old_base="$(git rev-parse HEAD)"
git merge -q --ff-only "$release_head"
git push -q origin main
git checkout -q --detach "$old_base"
plz_calls="$(wc -l <"$test_root/plz.log")"
run_adjustment
[[ $(wc -l <"$test_root/plz.log") == "$plz_calls" ]]
[[ $(git rev-parse HEAD) == "$old_base" ]]

# After the 1.0.3 merge, no remote commands or version changes are attempted.
git checkout -q main
gh_calls="$(wc -l <"$test_root/gh.log")"
run_adjustment
[[ $(wc -l <"$test_root/gh.log") == "$gh_calls" ]]
[[ $(wc -l <"$test_root/plz.log") == "$plz_calls" ]]
echo 'One-time release PR adjustment verified'
