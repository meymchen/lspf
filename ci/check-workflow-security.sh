#!/usr/bin/env bash
set -euo pipefail

workflow_dir="${1:-.github/workflows}"
policy_file="${2:-ci/workflow-permissions.json}"
yq_bin="${YQ_BIN:-yq}"
status=0

for command in "$yq_bin" jq; do
    if ! command -v "$command" >/dev/null; then
        echo "required command is unavailable: $command" >&2
        exit 2
    fi
done

is_pinned_container_reference() {
    [[ "$1" =~ ^[^@]+@sha256:[0-9a-fA-F]{64}$ ]]
}

check_action_reference() {
    local source="$1"
    local action="$2"

    if [[ "$action" == ./* ]] || \
        [[ "$action" =~ ^[^@]+@[0-9a-fA-F]{40}$ ]] || \
        { [[ "$action" == docker://* ]] && is_pinned_container_reference "${action#docker://}"; }; then
        return
    fi

    printf "%s: mutable Action '%s'; pin it to a full commit SHA or container digest\n" \
        "$source" "$action" >&2
    status=1
}

check_action_references() {
    local source="$1"
    local action_refs="$2"

    if [[ -z "$action_refs" ]]; then
        return
    fi

    while IFS= read -r action; do
        check_action_reference "$source" "$action"
    done <<<"$action_refs"
}

check_workflow_image() {
    local source="$1"
    local location="$2"
    local image="$3"

    if is_pinned_container_reference "$image"; then
        return
    fi

    printf "%s: %s image '%s' is mutable; pin it to a sha256 digest\n" \
        "$source" "$location" "$image" >&2
    status=1
}

while IFS= read -r workflow; do
    relative_workflow="${workflow#$workflow_dir/}"
    display_workflow=".github/workflows/$relative_workflow"
    jobs_json="$($yq_bin -o=json '.jobs' "$workflow")"

    action_refs="$($yq_bin -r '.jobs[] | (.uses, .steps[]?.uses) | select(. != null)' "$workflow")"
    check_action_references "$display_workflow" "$action_refs"

    while IFS=$'\t' read -r job image; do
        check_workflow_image "$display_workflow" "job '$job' container" "$image"
    done < <(jq -r 'to_entries[] | .key as $job | .value.container? |
        if type == "string" then [$job, .]
        elif .image? != null then [$job, .image]
        else empty end | @tsv' <<<"$jobs_json")

    while IFS=$'\t' read -r job service image; do
        check_workflow_image "$display_workflow" "job '$job' service '$service'" "$image"
    done < <(jq -r 'to_entries[] | .key as $job |
        ((.value.services? // {}) | to_entries[]) as $service |
        select($service.value.image? != null) |
        [$job, $service.key, $service.value.image] | @tsv' <<<"$jobs_json")

    while IFS= read -r job; do
        actual="$(jq -S -c --arg job "$job" '.[$job].permissions // null' <<<"$jobs_json")"

        if ! jq -e --arg workflow "$display_workflow" --arg job "$job" \
            '.workflows[$workflow] | has($job)' "$policy_file" >/dev/null; then
            printf "%s: job '%s' has no permission policy\n" \
                "$display_workflow" "$job" >&2
            status=1
            continue
        fi

        expected="$(jq -S -c --arg workflow "$display_workflow" --arg job "$job" \
            '.workflows[$workflow][$job]' "$policy_file")"

        if [[ "$actual" != "$expected" ]]; then
            printf "%s: job '%s' permissions violate policy; expected %s, found %s\n" \
                "$display_workflow" "$job" "$expected" "$actual" >&2
            status=1
        fi
    done < <(jq -r 'keys[]' <<<"$jobs_json")
done < <(find "$workflow_dir" -maxdepth 1 -type f \( -name '*.yml' -o -name '*.yaml' \) -print | sort)

while IFS= read -r action_file; do
    display_action_file="${action_file#./}"
    action_refs="$($yq_bin -r '.runs.steps[]?.uses | select(. != null)' "$action_file")"
    check_action_references "$display_action_file" "$action_refs"

    image="$($yq_bin -r '.runs.image // ""' "$action_file")"
    if [[ "$image" == docker://* ]]; then
        check_action_reference "$display_action_file" "$image"
    fi
done < <(find . -type d \( -name .git -o -name node_modules -o -name target \) -prune -o \
    -type f \( -name action.yml -o -name action.yaml \) -print | sort)

while IFS=$'\t' read -r policy_workflow job; do
    workflow="$workflow_dir/${policy_workflow#.github/workflows/}"
    if [[ ! -f "$workflow" ]] || \
        ! "$yq_bin" -o=json '.jobs' "$workflow" | jq -e --arg job "$job" 'has($job)' >/dev/null; then
        printf "%s: permission policy names missing job '%s'\n" \
            "$policy_workflow" "$job" >&2
        status=1
    fi
done < <(jq -r '.workflows | to_entries[] as $workflow | $workflow.value | keys[] |
    [$workflow.key, .] | @tsv' "$policy_file")

exit "$status"
