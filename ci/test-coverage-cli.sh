#!/usr/bin/env bash

test_coverage_parse_options() {
    local error_prefix=$1
    local usage_function=$2
    shift 2

    local -a option_names=()
    local -a option_variables=()
    while [[ ${1-} != -- ]]; do
        option_names+=("${1:?missing option name}")
        option_variables+=("${2:?missing option variable}")
        shift 2
    done
    shift

    local argument option_index matched
    while (($#)); do
        argument=$1
        matched=false
        for option_index in "${!option_names[@]}"; do
            if [[ $argument == "${option_names[$option_index]}" ]]; then
                [[ $# -ge 2 ]] || {
                    echo "$error_prefix error: $argument requires a value" >&2
                    exit 2
                }
                printf -v "${option_variables[$option_index]}" '%s' "$2"
                shift 2
                matched=true
                break
            fi
        done
        $matched && continue

        case $argument in
            --help|-h)
                "$usage_function"
                exit 0
                ;;
            *)
                echo "$error_prefix error: unknown argument '$argument'" >&2
                "$usage_function" >&2
                exit 2
                ;;
        esac
    done
}
