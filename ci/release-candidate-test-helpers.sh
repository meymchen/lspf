#!/usr/bin/env bash

create_release_candidate_fixture() {
    local revision=$1
    local artifact_dir=$2
    local evidence_dir=$3
    local docs_root gate gate_dir file

    mkdir -p "$artifact_dir" "$evidence_dir"

    for file in \
        lspf-1.0.0.crate \
        CHANGELOG.md \
        lspf-CHANGELOG.md \
        lspf-1.0.0.spdx.json
    do
        printf 'fixture for %s\n' "$file" >"$artifact_dir/$file"
    done

    docs_root="$(mktemp -d)"
    jq -n --arg revision "$revision" '{
        schemaVersion: 1,
        crate: "lspf",
        version: "1.0.0",
        revision: $revision
      }' >"$docs_root/release-docs-metadata.json"
    tar -czf "$artifact_dir/lspf-1.0.0-docs.tar.gz" \
        -C "$docs_root" release-docs-metadata.json
    rm -rf "$docs_root"

    jq -n --arg revision "$revision" '{
        schemaVersion: 1,
        crate: "lspf",
        version: "1.0.0",
        revision: $revision,
        tag: "v1.0.0",
        sourceRepository: "https://github.com/meymchen/lspf",
        workflowRun: "https://github.com/meymchen/lspf/actions/runs/123",
        artifacts: {
          crate: "lspf-1.0.0.crate",
          docs: "lspf-1.0.0-docs.tar.gz",
          changelogs: ["CHANGELOG.md", "lspf-CHANGELOG.md"],
          sbom: "lspf-1.0.0.spdx.json",
          hashes: "SHA256SUMS",
          provenance: "provenance.jsonl",
          sbomAttestation: "sbom-attestation.jsonl"
        }
      }' >"$artifact_dir/release-metadata.json"

    for gate in A B C D; do
        gate_dir="$evidence_dir/gate-${gate,,}"
        mkdir -p "$gate_dir"
        jq -n --arg gate "$gate" --arg revision "$revision" '{
            schemaVersion: 1,
            gate: $gate,
            revision: $revision,
            workflowRun: "https://github.com/meymchen/lspf/actions/runs/123",
            overallResult: "success",
            failedChecks: (if $gate == "D" then null else [] end),
            failedComponents: (if $gate == "D" then [] else null end),
            humanJudgments: (
              if $gate == "A" then [{
                classification: "human",
                statement: "Maintainers decide whether the support promise is acceptable."
              }] else [] end
            )
          }' >"$gate_dir/evidence.json"
        printf '# Gate %s evidence\n' "$gate" >"$gate_dir/evidence.md"
    done
}
