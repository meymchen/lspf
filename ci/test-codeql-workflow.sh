#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=ci/workflow-test-helpers.sh
source "$repo_root/ci/workflow-test-helpers.sh"

workflow="$repo_root/.github/workflows/codeql.yml"
workflow_json="$(workflow_yaml_to_json "$workflow")"

assert_contract() {
    local query="$1"
    local message="$2"

    if ! jq -e "$query" <<<"$workflow_json" >/dev/null; then
        echo "$message" >&2
        exit 1
    fi
}

# The reason this workflow exists instead of code scanning's default setup. A
# default setup cannot express `paths-ignore`, and without these exclusions the
# Rust extractor resolves three Cargo workspaces instead of one.
assert_contract '
    .jobs.analyze.strategy.matrix.include
    | map(select(.language == "rust"))
    | length == 1 and (.[0].config | type) == "string"
' 'the Rust analysis must carry an inline CodeQL config'

assert_contract '
    .jobs.analyze.strategy.matrix.include
    | map(select(.language == "rust"))[0].config as $config
    | ($config | contains("paths-ignore"))
      and ($config | contains("- fuzz"))
      and ($config | contains("- editor-validation"))
' 'the Rust analysis must exclude the fuzz and editor-validation Cargo workspaces'

# `paths-ignore` is only honoured for languages analyzed without a build, so the
# exclusion above silently stops applying if the Rust build mode ever changes.
assert_contract '
    .jobs.analyze.strategy.matrix.include
    | all(.["build-mode"] == "none")
' 'every language must be analyzed with build-mode: none'

# Reaching the config through the matrix is what keeps the exclusions scoped to
# Rust; a hardcoded `config:` would apply them to every extractor.
assert_contract '
    .jobs.analyze.steps
    | map(select((.uses // "") | startswith("github/codeql-action/init@")))
    | length == 1
      and .[0].with.config == "${{ matrix.config }}"
      and .[0].with["build-mode"] == "${{ matrix.build-mode }}"
' 'CodeQL init must take its config and build mode from the matrix'

# Matches the language set the superseded default setup was configured with.
assert_contract '
    .jobs.analyze.strategy.matrix.include
    | map(.language)
    | sort == ["actions", "javascript-typescript", "rust"]
' 'the analysis must cover the actions, javascript-typescript, and rust languages'

assert_contract \
    '.jobs.analyze.strategy["fail-fast"] == false' \
    'one language failing must not cancel the analysis of the others'

assert_contract '
    .jobs.analyze.permissions
    | .["security-events"] == "write" and .contents == "read"
' 'the analysis needs security-events: write to upload results and contents: read to check out'

assert_contract '
    .jobs.analyze.steps
    | map(select((.uses // "") | startswith("github/codeql-action/analyze@")))
    | length == 1 and .[0].with.category == "/language:${{ matrix.language }}"
' 'each language must upload its results under a distinct category'

# Alerts on `main` only resolve when the default branch is re-analyzed, so a
# pull-request-only trigger would leave fixed alerts open until the weekly run.
assert_contract '
    (.on // .["true"]) as $triggers
    | $triggers.push.branches == ["main"]
      and ($triggers | has("pull_request"))
      and ($triggers | has("schedule"))
' 'the analysis must run on main, on pull requests, and on a schedule'

echo 'CodeQL workflow contract verified'
