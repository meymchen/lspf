#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workflow="$repo_root/.github/workflows/sonarcloud.yml"
checkout_ref="actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803"
workflow_json="$(yq -o=json '.' "$workflow")"

assert_contract() {
    local query="$1"
    local message="$2"

    if ! jq -e --arg checkout_ref "$checkout_ref" "$query" <<<"$workflow_json" >/dev/null; then
        echo "$message" >&2
        exit 1
    fi
}

assert_contract \
    '.jobs.Analysis.permissions.contents == "read"' \
    'SonarCloud analysis needs contents: read to check out Git history'

assert_contract '
    .jobs.Analysis.steps as $steps
    | [$steps[] | select(.uses == $checkout_ref)] as $checkouts
    | ($checkouts | length) == 1
      and $checkouts[0].with["fetch-depth"] == 0
' 'SonarCloud analysis must check out the complete Git history exactly once'

assert_contract '
    .jobs.Analysis.steps as $steps
    | ([range(0; $steps | length) | select($steps[.].uses == $checkout_ref)] | first) as $checkout
    | ([range(0; $steps | length) |
        select(($steps[.].uses // "") | startswith("SonarSource/sonarcloud-github-action@"))]
        | first) as $analysis
    | $checkout != null and $analysis != null and $checkout < $analysis
' 'SonarCloud analysis must check out Git history before running the scanner'

assert_contract '
    .jobs.Analysis.steps as $steps
    | ([range(0; $steps | length) |
        select(($steps[.].run // "") |
            contains("npm --prefix tools/vscode-test-client run test:coverage"))]
        | first) as $coverage
    | ([range(0; $steps | length) |
        select(($steps[.].uses // "") | startswith("SonarSource/sonarcloud-github-action@"))]
        | first) as $analysis
    | $coverage != null and $analysis != null and $coverage < $analysis
' 'SonarCloud analysis must generate TypeScript coverage before running the scanner'

assert_contract '
    .jobs.Analysis.steps
    | any((.run // "") |
        contains("SF:tools/vscode-test-client/"))
' 'SonarCloud analysis must make LCOV source paths relative to the repository root'

assert_contract '
    .jobs.Analysis.steps
    | map(select((.uses // "") | startswith("SonarSource/sonarcloud-github-action@")))
    | length == 1
      and (.[0].with.args |
        contains("-Dsonar.javascript.lcov.reportPaths=coverage/vscode-test-client/lcov.info"))
' 'SonarCloud analysis must import the generated TypeScript LCOV report'

echo 'SonarCloud workflow contract verified'
