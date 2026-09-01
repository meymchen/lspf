#!/usr/bin/env bash
set -euo pipefail

version="${1:?usage: authorize-release.sh VERSION REPOSITORY BASE_BRANCH}"
repository="${2:?usage: authorize-release.sh VERSION REPOSITORY BASE_BRANCH}"
base_branch="${3:?usage: authorize-release.sh VERSION REPOSITORY BASE_BRANCH}"
expected_title="chore: release v$version"

if [[ -n ${RELEASE_PRS_FILE:-} ]]; then
    release_prs="$(jq -c '.' "$RELEASE_PRS_FILE")"
else
    release_pr_pages="$(
        gh api --paginate --slurp \
            "repos/$repository/pulls?state=closed&base=$base_branch&sort=updated&direction=desc&per_page=100"
    )"
    release_prs="$(jq -c 'add' <<<"$release_pr_pages")"
fi

authorized=false
authorized_pr=
candidates="$(
    jq -r \
        --arg repository "$repository" \
        --arg title "$expected_title" '
        .[]
        | select(
            .merged_at != null
            and .title == $title
            and .head.repo.full_name == $repository
            and (.head.ref | startswith("release-plz-"))
        )
        | [.number, .merge_commit_sha]
        | @tsv
        ' <<<"$release_prs"
)"
while IFS=$'\t' read -r number merge_commit; do
    number="${number%$'\r'}"
    merge_commit="${merge_commit%$'\r'}"
    if [[ -n $merge_commit ]] && git merge-base --is-ancestor "$merge_commit" HEAD; then
        authorized=true
        authorized_pr="$number"
        break
    fi
done <<<"$candidates"

if [[ -n ${GITHUB_OUTPUT:-} ]]; then
    {
        printf 'authorized=%s\n' "$authorized"
        printf 'pull-request=%s\n' "$authorized_pr"
    } >>"$GITHUB_OUTPUT"
fi

if [[ $authorized == true ]]; then
    printf 'Release v%s is authorized by merged release-plz PR #%s\n' \
        "$version" "$authorized_pr"
else
    printf 'No merged release-plz PR authorizes v%s in the current history\n' \
        "$version"
fi
