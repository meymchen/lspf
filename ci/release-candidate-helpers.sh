#!/usr/bin/env bash

# Read the crate identity a release bundle was built around into `crate_name`,
# `crate_version`, `crate_file`, and `package_root`. `jq` on Windows writes CRLF
# to a pipe, so every field is trimmed before it reaches a path.
read_release_crate_identity() {
    local release_metadata=$1

    crate_name="$(jq -r '.crate' "$release_metadata")"
    crate_version="$(jq -r '.version' "$release_metadata")"
    crate_file="$(jq -r '.artifacts.crate' "$release_metadata")"
    crate_name="${crate_name%$'\r'}"
    crate_version="${crate_version%$'\r'}"
    crate_file="${crate_file%$'\r'}"
    package_root="$crate_name-$crate_version"
}

# A candidate is only usable downstream when its own metadata names the
# validated revision and reports all four automated gates green.
validate_candidate_metadata() {
    local revision=$1
    local candidate_metadata=$2

    if ! jq -e --arg revision "$revision" '
        .schemaVersion == 1
        and .revision == $revision
        and ([.gates[].gate] == ["A", "B", "C", "D"])
        and all(.gates[]; .result == "success")
      ' "$candidate_metadata" >/dev/null 2>&1
    then
        echo 'candidate metadata is missing, malformed, failing, or names another revision' >&2
        return 1
    fi
}

# Each gate names its failures under a different key: the check-shaped gates use
# `failedChecks`, Gate D groups verification components, and Gate E groups
# candidate journeys.
release_gate_failure_key() {
    case "$1" in
        D) printf 'failedComponents\n' ;;
        E) printf 'failedJourneys\n' ;;
        *) printf 'failedChecks\n' ;;
    esac
}

validate_release_gate_evidence() {
    local revision=$1
    local evidence_dir=$2
    shift 2
    local gates=("$@")
    local gate gate_file failure_key

    if ((${#gates[@]} == 0)); then
        gates=(A B C D)
    fi

    for gate in "${gates[@]}"; do
        gate_file="$evidence_dir/gate-${gate,,}/evidence.json"
        failure_key="$(release_gate_failure_key "$gate")"
        if ! jq -e \
            --arg gate "$gate" \
            --arg revision "$revision" \
            --arg key "$failure_key" '
            .schemaVersion == 1
            and .gate == $gate
            and .revision == $revision
            and .overallResult == "success"
            and (.[$key] | type == "array" and length == 0)
          ' "$gate_file" >/dev/null 2>&1
        then
            printf 'Gate %s evidence is missing, malformed, failing, or names another revision\n' \
                "$gate" >&2
            return 1
        fi
    done
}
