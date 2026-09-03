#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo_root/ci/workflow-test-helpers.sh"
workflow="$repo_root/.github/workflows/ci.yml"
security_workflow="$repo_root/.github/workflows/security.yml"
permissions_policy="$repo_root/ci/workflow-permissions.json"
release_plz_config="$repo_root/release-plz.toml"

workflow_json="$(workflow_yaml_to_json "$workflow")"
gate_d_job="$(jq -c '.jobs["gate-d-candidate-evidence"]' <<<"$workflow_json")"
candidate_job="$(jq -c '.jobs["release-candidate"]' <<<"$workflow_json")"

jq -e '
  .name == "Gate D candidate evidence"
  and .if == "${{ github.event_name == '\''push'\'' && needs.release-context.outputs.authorized == '\''true'\'' && needs.release-context.outputs.version == '\''1.0.0'\'' }}"
  and .needs == "release-context"
  and .["runs-on"] == "ubuntu-latest"
  and .["timeout-minutes"] == 60
  and .permissions == {"contents": "read"}
  and any(.steps[];
    .uses == "./.github/actions/setup-rust"
    and .with.toolchain == "nightly"
    and .with.cache == "false")
  and any(.steps[];
    .name == "Run revision-locked Gate D candidate verification"
    and (.run | contains("bash ci/run-gate-d-evidence.sh"))
    and (.run | contains("$GITHUB_SHA"))
    and (.run | contains("$GITHUB_RUN_ID")))
  and any(.steps[];
    .name == "Retain Gate D candidate evidence"
    and ((.uses // "") | startswith("actions/upload-artifact@"))
    and .with.name == "gate-d-candidate-evidence"
    and .with.path == "${{ runner.temp }}/gate-d-candidate-evidence"
    and .with["if-no-files-found"] == "error"
    and .with["retention-days"] == 90)
' <<<"$gate_d_job" >/dev/null

jq -e '
  .name == "Verified release candidate"
  and .if == "${{ github.event_name == '\''push'\'' && needs.release-context.outputs.authorized == '\''true'\'' && needs.release-context.outputs.version == '\''1.0.0'\'' }}"
  and (.needs | sort) == [
    "gate-a-evidence",
    "gate-b-evidence",
    "gate-c-evidence",
    "gate-d-candidate-evidence",
    "release-context"
  ]
  and .permissions == {
    "actions": "read",
    "attestations": "write",
    "contents": "read",
    "id-token": "write"
  }
  and ([.steps[] | select((.uses? // "") | startswith("actions/checkout@"))] | length) == 1
  and ([.steps[] | select((.uses? // "") | startswith("actions/checkout@"))][0].with | has("ref") | not)
  and any(.steps[];
    .name == "Download Gate A through D evidence"
    and (.run | contains("gate-a-release-evidence"))
    and (.run | contains("gate-b-bounded-resource-evidence"))
    and (.run | contains("gate-c-endpoint-evidence"))
    and (.run | contains("gate-d-candidate-evidence"))
    and (.run | contains("$GITHUB_RUN_ID")))
  and any(.steps[];
    .name == "Prepare release artifacts from the validated revision"
    and .id == "artifacts"
    and (.run | contains("bash ci/prepare-release-artifacts.sh"))
    and (.run | contains("$GITHUB_SHA")))
  and any(.steps[];
    .name == "Prepare the verified candidate"
    and (.run | contains("bash ci/prepare-release-candidate.sh"))
    and (.run | contains("$GITHUB_SHA")))
  and any(.steps[];
    .name == "Generate crate SBOM"
    and ((.uses? // "") | startswith("anchore/sbom-action@"))
    and .with.file == "${{ steps.artifacts.outputs.crate }}"
    and .with["output-file"] == "${{ steps.artifacts.outputs.sbom }}"
    and .with["upload-artifact"] == false
    and .with["upload-release-assets"] == false)
  and any(.steps[];
    .name == "Generate candidate provenance"
    and .id == "provenance"
    and ((.uses? // "") | startswith("actions/attest@"))
    and (.with["subject-path"] | contains("${{ steps.artifacts.outputs.docs }}"))
    and (.with["subject-path"] | contains("candidate-metadata.json"))
    and (.with["subject-path"] | contains("candidate.md"))
    and (.with["subject-path"] | contains("evidence")))
  and any(.steps[];
    .name == "Generate SBOM attestation"
    and .id == "sbom-attestation"
    and ((.uses? // "") | startswith("actions/attest@"))
    and .with["subject-path"] == "${{ steps.artifacts.outputs.crate }}"
    and .with["sbom-path"] == "${{ steps.artifacts.outputs.sbom }}")
  and any(.steps[];
    .name == "Verify candidate hashes and clean installation"
    and (.run | contains("bash ci/check-release-candidate.sh"))
    and (.run | contains("$GITHUB_SHA")))
  and any(.steps[];
    .name == "Retain verified release candidate"
    and ((.uses // "") | startswith("actions/upload-artifact@"))
    and .with.name == "lspf-${{ steps.artifacts.outputs.version }}-release-candidate"
    and .with.path == "${{ steps.artifacts.outputs.directory }}"
    and .with["if-no-files-found"] == "error"
    and .with["retention-days"] == 90)
  and all(.steps[];
    ((.uses? // "") | startswith("release-plz/action@") | not)
    and ((.run? // "") | contains("gh release") | not))
' <<<"$candidate_job" >/dev/null

prepare_index="$(jq -r '.steps | map(.name) | index("Prepare the verified candidate")' \
    <<<"$candidate_job")"
provenance_index="$(jq -r '.steps | map(.name) | index("Generate candidate provenance")' \
    <<<"$candidate_job")"
hash_index="$(jq -r '.steps | map(.name) | index("Retain attestations and hash every candidate file")' \
    <<<"$candidate_job")"
verify_index="$(jq -r '.steps | map(.name) | index("Verify candidate hashes and clean installation")' \
    <<<"$candidate_job")"
upload_index="$(jq -r '.steps | map(.name) | index("Retain verified release candidate")' \
    <<<"$candidate_job")"
((prepare_index < provenance_index))
((provenance_index < hash_index))
((hash_index < verify_index))
((verify_index < upload_index))

# Candidate construction and publication are separate decisions. The ordinary
# release-plz job may still open the version/changelog PR, but this workflow
# must not publish it or rewrite the release_always policy.
jq -e '
  (.jobs | has("release-plz-release") | not)
  and any(.jobs["release-plz-pr"].steps[];
    ((.uses? // "") | startswith("release-plz/action@"))
    and .with.command == "release-pr")
' <<<"$workflow_json" >/dev/null

workflow_yaml_to_json "$security_workflow" | jq -e '
  .jobs["supply-chain"].steps[] |
  select(.run == "bash ci/test-release-artifacts-workflow.sh")
' >/dev/null

jq -e '
  .workflows[".github/workflows/ci.yml"]["gate-d-candidate-evidence"]
    == {"contents": "read"}
  and .workflows[".github/workflows/ci.yml"]["release-candidate"] == {
    "actions": "read",
    "attestations": "write",
    "contents": "read",
    "id-token": "write"
  }
  and (.workflows[".github/workflows/ci.yml"] | has("release-plz-release") | not)
' "$permissions_policy" >/dev/null

yq -p toml -o json '.' "$release_plz_config" | jq -e '
  .workspace.release_always == false
  and any(.package[]; .name == "lspf" and .git_tag_name == "v{{ version }}")
' >/dev/null

semver_checks_tool='cargo-semver-checks@0.50.0'
jq -e --arg semver "$semver_checks_tool" '
  any(.jobs["release-plz-pr"].steps[];
    .uses == "./.github/actions/setup-rust" and .with.tools == $semver)
  and any(.jobs["public-api"].steps[];
    .uses == "./.github/actions/setup-rust" and .with.tools == $semver)
' <<<"$workflow_json" >/dev/null

echo 'Verified release candidate workflow contract verified'
