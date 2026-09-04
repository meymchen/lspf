#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

empty_manifest="$test_root/no-gaps.json"
jq -n '{frameworkGaps: {tracked: [], untracked: []}}' >"$empty_manifest"

expect_rejected() {
    local description=$1
    local register=$2
    local manifest=$3
    local expected=$4

    if bash ci/check-release-blockers.sh "$register" "$manifest" \
        >"$test_root/output" 2>&1
    then
        printf 'test failure: %s passed the register check\n' "$description" >&2
        exit 1
    fi
    grep -F "$expected" "$test_root/output" >/dev/null
}

# The register the repository ships must satisfy its own rules.
bash ci/check-release-blockers.sh >/dev/null

blocker() {
    jq -n --argjson overrides "$1" '{
        id: "outbound-broker-leak",
        severity: "P1",
        owner: "framework",
        statement: "The outbound broker retains a cancelled request slot.",
        issue: "https://github.com/meymchen/lspf/issues/4242",
        disposition: "resolved"
      } * $overrides
      | {schemaVersion: 1, blockers: [.]}'
}

blocker '{}' >"$test_root/resolved.json"
bash ci/check-release-blockers.sh "$test_root/resolved.json" \
    "$empty_manifest" >/dev/null

blocker '{"disposition": "open", "justification": "not yet fixed"}' \
    >"$test_root/open.json"
expect_rejected 'an open framework P1' "$test_root/open.json" \
    "$empty_manifest" 'undisposed framework-owned P0 or P1 blockers remain'

blocker '{"severity": "P0", "disposition": "open", "justification": "not yet fixed"}' \
    >"$test_root/open-p0.json"
expect_rejected 'an open framework P0' "$test_root/open-p0.json" \
    "$empty_manifest" 'undisposed framework-owned P0 or P1 blockers remain'

# A disposition that records a maintainer decision is allowed; a lower-severity
# or downstream-owned entry never blocks the release on its own.
blocker '{"disposition": "accepted", "justification": "latency only"}' \
    >"$test_root/accepted.json"
bash ci/check-release-blockers.sh "$test_root/accepted.json" \
    "$empty_manifest" | grep -F '1 framework P0/P1 accepted' >/dev/null

blocker '{"severity": "P2", "disposition": "open", "justification": "deferred"}' \
    >"$test_root/p2.json"
bash ci/check-release-blockers.sh "$test_root/p2.json" \
    "$empty_manifest" >/dev/null

blocker '{"owner": "downstream", "disposition": "open", "justification": "deferred"}' \
    >"$test_root/downstream.json"
bash ci/check-release-blockers.sh "$test_root/downstream.json" \
    "$empty_manifest" >/dev/null

# Shape violations are rejected rather than silently ignored.
blocker '{"disposition": "accepted"}' >"$test_root/unjustified.json"
expect_rejected 'an accepted blocker without a justification' \
    "$test_root/unjustified.json" "$empty_manifest" \
    'release blocker register is missing or malformed'

blocker '{"issue": "https://example.invalid/issues/1"}' \
    >"$test_root/untracked-issue.json"
expect_rejected 'a blocker outside the issue tracker' \
    "$test_root/untracked-issue.json" "$empty_manifest" \
    'release blocker register is missing or malformed'

blocker '{"severity": "critical"}' >"$test_root/bad-severity.json"
expect_rejected 'an unrecognized severity' "$test_root/bad-severity.json" \
    "$empty_manifest" 'release blocker register is missing or malformed'

jq -n '{schemaVersion: 2, blockers: []}' >"$test_root/bad-schema.json"
expect_rejected 'an unrecognized schema version' "$test_root/bad-schema.json" \
    "$empty_manifest" 'release blocker register is missing or malformed'

jq -s '{schemaVersion: 1, blockers: [.[0].blockers[0], .[0].blockers[0]]}' \
    "$test_root/resolved.json" >"$test_root/duplicate.json"
expect_rejected 'a duplicate blocker id' "$test_root/duplicate.json" \
    "$empty_manifest" 'release blocker register is missing or malformed'

expect_rejected 'a missing register' "$test_root/absent.json" \
    "$empty_manifest" 'missing release blocker input'

# A gap the editor journeys tracked must be dispositioned here as well.
gap_manifest="$test_root/tracked-gap.json"
jq -n '{
    frameworkGaps: {
      tracked: [{issue: "https://github.com/meymchen/lspf/issues/9001"}],
      untracked: []
    }
  }' >"$gap_manifest"
expect_rejected 'an editor gap absent from the register' \
    "$test_root/resolved.json" "$gap_manifest" \
    'does not disposition'

blocker '{"issue": "https://github.com/meymchen/lspf/issues/9001"}' \
    >"$test_root/dispositioned-gap.json"
bash ci/check-release-blockers.sh "$test_root/dispositioned-gap.json" \
    "$gap_manifest" >/dev/null

echo 'Release blocker register diagnostics verified'
