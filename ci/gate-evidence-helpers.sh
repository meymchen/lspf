#!/usr/bin/env bash

# Run one command, appending it to the human-readable log and to the machine
# list the evidence assembler joins into the recorded `command` field. Anything
# that changes the meaning of a run — an environment variable a journey depends
# on, for instance — belongs in the argument list rather than around it, so the
# recorded command stays reproducible.
run_logged() {
    local log=$1
    local commands_file=$2
    shift 2
    local rendered

    printf -v rendered '%q ' "$@"
    rendered=${rendered% }
    printf '%s\n' "$rendered" >>"$commands_file"
    printf '$ %s\n' "$rendered" >>"$log"
    "$@"
}

# Join the commands one run issued into the single string the evidence records.
joined_logged_commands() {
    awk 'NR == 1 {combined=$0; next} {combined=combined " && " $0} END {print combined}' \
        "$1"
}
