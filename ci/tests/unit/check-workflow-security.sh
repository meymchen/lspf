#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT

workflow_dir="$fixture_dir/workflows"
policy_file="$fixture_dir/workflow-permissions.json"
mkdir -p "$workflow_dir"

write_workflow() {
    local permissions="$1"
    local job_configuration="${2:-}"
    local steps="${3:-    steps: []}"

    {
        printf '%s\n' \
            'name: Security fixture' \
            'on: push' \
            'permissions: {}' \
            'jobs:' \
            '  audit:' \
            '    runs-on: ubuntu-latest' \
            '    permissions:' \
            "      $permissions"
        if [[ -n "$job_configuration" ]]; then
            printf '%s\n' "$job_configuration"
        fi
        printf '%s\n' "$steps"
    } >"$workflow_dir/fixture.yml"
}

assert_failure() {
    local expected="$1"
    local description="$2"
    local output

    if output="$(bash "$repo_root/ci/check-workflow-security.sh" "$workflow_dir" "$policy_file" 2>&1)"; then
        printf 'expected %s to fail\n' "$description" >&2
        exit 1
    fi
    grep -F "$expected" <<<"$output" >/dev/null
}

write_policy() {
    local permissions="$1"
    local extra_policy="${2:-}"

    jq -n --argjson permissions "$permissions" --arg extra_policy "$extra_policy" '
        {workflows: {".github/workflows/fixture.yml": {audit: $permissions}}}
        | if $extra_policy == "future-job" then
            .workflows[".github/workflows/fixture.yml"]["future-job"] = {}
          else . end
    ' >"$policy_file"
}

write_policy '{}'
write_workflow '{}'
bash "$repo_root/ci/check-workflow-security.sh" "$workflow_dir" "$policy_file"

write_policy '{}'
write_workflow 'contents: read'
assert_failure "job 'audit' permissions violate policy; expected {}, found {\"contents\":\"read\"}" \
    'an unnecessary contents permission'

write_policy '{}'
write_workflow '{}' $'    steps: []\n  future-job:\n    runs-on: ubuntu-latest\n    permissions: {}\n    steps: []'
assert_failure "job 'future-job' has no permission policy" 'a job missing from the policy'

write_policy '{}' 'future-job'
write_workflow '{}'
assert_failure "permission policy names missing job 'future-job'" 'a stale policy entry'

write_policy '{}'
write_workflow '{}' '' $'    steps:\n      - uses: actions/checkout@v6'
assert_failure "mutable Action 'actions/checkout@v6'" 'a mutable Action reference'

write_policy '{"contents":"read"}'
write_workflow 'contents: read' '' $'    steps:\n      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1'
bash "$repo_root/ci/check-workflow-security.sh" "$workflow_dir" "$policy_file"

write_policy '{}'
write_workflow '{}' '    container: ubuntu:latest'
assert_failure "job 'audit' container image 'ubuntu:latest' is mutable; pin it to a sha256 digest" \
    'a mutable job container image'

write_workflow '{}' $'    services:\n      database:\n        image: postgres:latest'
assert_failure "job 'audit' service 'database' image 'postgres:latest' is mutable; pin it to a sha256 digest" \
    'a mutable service image'

write_workflow '{}' '    container: ubuntu@sha256:2222222222222222222222222222222222222222222222222222222222222222'
bash "$repo_root/ci/check-workflow-security.sh" "$workflow_dir" "$policy_file"
