#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo_root/ci/workflow-test-helpers.sh"
ci_workflow="$repo_root/.github/workflows/ci.yml"
security_workflow="$repo_root/.github/workflows/security.yml"
permissions_policy="$repo_root/ci/workflow-permissions.json"

ci_json="$(workflow_yaml_to_json "$ci_workflow")"
job="$(jq -c '.jobs["release-publish"]' <<<"$ci_json")"

# Publishing must stay downstream of every gate, behind a protected
# environment, and it must never be reachable from a pull request.
jq -e '
  .name == "Publish the verified candidate"
  and .if == "${{ github.event_name == '\''push'\'' && needs.release-context.outputs.authorized == '\''true'\'' }}"
  and (.needs | sort)
    == ["gate-e-evidence", "release-candidate", "release-context"]
  and .environment == "crates-io"
  and .permissions == {
    "actions": "read",
    "contents": "write",
    "id-token": "write"
  }
' <<<"$job" >/dev/null

# The publication must come from the validated revision, be minted a
# short-lived registry token, and then be checked against the registry rather
# than trusted.
jq -e '
  any(.steps[];
    ((.uses // "") | startswith("actions/checkout@"))
    and .with["fetch-depth"] == 0
    and .with["persist-credentials"] == false)
  and any(.steps[];
    .name == "Download the verified candidate and its Gate E evidence"
    and (.run | contains("gh run download"))
    and (.run | contains("$RUNNER_TEMP/candidate"))
    and (.run | contains("gate-e-candidate-validation-evidence")))
  and any(.steps[];
    .name == "Prove this revision repackages to the validated candidate"
    and (.run | contains("bash ci/check-repackaged-candidate.sh"))
    and (.run | contains("$GITHUB_SHA"))
    and (.run | contains("$RUNNER_TEMP/candidate/lspf-$RELEASE_VERSION.crate")))
  and any(.steps[];
    .id == "registry"
    and ((.uses // "") | startswith("rust-lang/crates-io-auth-action@")))
  and any(.steps[];
    .name == "Publish the validated revision"
    and .run == "cargo publish -p lspf --locked"
    and .env.CARGO_REGISTRY_TOKEN == "${{ steps.registry.outputs.token }}")
  and any(.steps[];
    .name == "Download the crate the registry now serves"
    and (.run | contains("https://static.crates.io/crates/lspf/"))
    and (.run | contains("$RUNNER_TEMP/published.crate")))
  and any(.steps[];
    .name == "Archive the release record"
    and (.run | contains("bash ci/prepare-release-record.sh"))
    and (.run | contains("$GITHUB_SHA"))
    and (.run | contains("$RUNNER_TEMP/candidate"))
    and (.run | contains("$RUNNER_TEMP/gate-e-evidence"))
    and (.run | contains("$RUNNER_TEMP/published.crate"))
    and (.run | contains("$RUNNER_TEMP/release-record")))
  and any(.steps[];
    .name == "Verify the archived release record"
    and (.run | contains("bash ci/check-release-record.sh"))
    and (.run | contains("$GITHUB_SHA"))
    and (.run | contains("$RUNNER_TEMP/release-record")))
  and any(.steps[];
    .name == "Retain the release record"
    and ((.uses // "") | startswith("actions/upload-artifact@"))
    and (.with.name | contains("release-record"))
    and .with.path == "${{ runner.temp }}/release-record"
    and .with["if-no-files-found"] == "error")
  and any(.steps[];
    .name == "Tag the release and attach the record"
    and (.run | contains("gh release create"))
    and (.run | contains("--target \"$GITHUB_SHA\""))
    and .env.RELEASE_TAG
      == "v${{ needs.release-context.outputs.version }}")
' <<<"$job" >/dev/null

# The repackaging comparison only prevents anything if it runs before the token
# is minted and the crate is uploaded.
jq -e '
  [.steps[].name] as $names
  | ($names | index("Prove this revision repackages to the validated candidate"))
    < ($names | index("Mint a short-lived crates.io token"))
  and ($names | index("Mint a short-lived crates.io token"))
    < ($names | index("Publish the validated revision"))
  and ($names | index("Publish the validated revision"))
    < ($names | index("Download the crate the registry now serves"))
' <<<"$job" >/dev/null

# release-plz still proposes the version and changelog, and still must not be
# the thing that publishes them.
jq -e '
  (.jobs | has("release-plz-release") | not)
  and any(.jobs["release-plz-pr"].steps[];
    ((.uses? // "") | startswith("release-plz/action@"))
    and .with.command == "release-pr")
' <<<"$ci_json" >/dev/null

workflow_yaml_to_json "$security_workflow" | jq -e '
  .jobs["supply-chain"].steps[] |
  select(.run == "bash ci/test-check-repackaged-candidate.sh")
' >/dev/null

workflow_yaml_to_json "$security_workflow" | jq -e '
  .jobs["supply-chain"].steps[] |
  select(.run == "bash ci/test-prepare-release-record.sh")
' >/dev/null

workflow_yaml_to_json "$security_workflow" | jq -e '
  .jobs["supply-chain"].steps[] |
  select(.run == "bash ci/test-release-publication-workflow.sh")
' >/dev/null

jq -e '
  .workflows[".github/workflows/ci.yml"]["release-publish"] == {
    "actions": "read",
    "contents": "write",
    "id-token": "write"
  }
' "$permissions_policy" >/dev/null

echo 'Release publication workflow contract verified'
