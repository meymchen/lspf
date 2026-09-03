#!/usr/bin/env bash

validate_release_gate_evidence() {
    local revision=$1
    local evidence_dir=$2
    local gate gate_file

    for gate in A B C D; do
        gate_file="$evidence_dir/gate-${gate,,}/evidence.json"
        if ! jq -e --arg gate "$gate" --arg revision "$revision" '
            .schemaVersion == 1
            and .gate == $gate
            and .revision == $revision
            and .overallResult == "success"
            and if $gate == "D" then
              (.failedComponents | type == "array" and length == 0)
            else
              (.failedChecks | type == "array" and length == 0)
            end
          ' "$gate_file" >/dev/null 2>&1
        then
            printf 'Gate %s evidence is missing, malformed, failing, or names another revision\n' \
                "$gate" >&2
            return 1
        fi
    done
}
