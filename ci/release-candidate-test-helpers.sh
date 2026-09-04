#!/usr/bin/env bash

# The fourth argument is the release version, defaulting to the 1.0.0 the
# existing callers build. It exists so a test can prove the candidate pipeline
# carries whatever version the authorized release bumped to.
create_release_candidate_fixture() {
    local revision=$1
    local artifact_dir=$2
    local evidence_dir=$3
    local version=${4:-1.0.0}
    local docs_root gate gate_dir file

    mkdir -p "$artifact_dir" "$evidence_dir"

    for file in \
        "lspf-$version.crate" \
        CHANGELOG.md \
        lspf-CHANGELOG.md \
        "lspf-$version.spdx.json"
    do
        printf 'fixture for %s\n' "$file" >"$artifact_dir/$file"
    done

    docs_root="$(mktemp -d)"
    jq -n --arg revision "$revision" --arg version "$version" '{
        schemaVersion: 1,
        crate: "lspf",
        version: $version,
        revision: $revision
      }' >"$docs_root/release-docs-metadata.json"
    tar -czf "$artifact_dir/lspf-$version-docs.tar.gz" \
        -C "$docs_root" release-docs-metadata.json
    rm -rf "$docs_root"

    jq -n --arg revision "$revision" --arg version "$version" '{
        schemaVersion: 1,
        crate: "lspf",
        version: $version,
        revision: $revision,
        tag: ("v" + $version),
        sourceRepository: "https://github.com/meymchen/lspf",
        workflowRun: "https://github.com/meymchen/lspf/actions/runs/123",
        artifacts: {
          crate: ("lspf-" + $version + ".crate"),
          docs: ("lspf-" + $version + "-docs.tar.gz"),
          changelogs: ["CHANGELOG.md", "lspf-CHANGELOG.md"],
          sbom: ("lspf-" + $version + ".spdx.json"),
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
