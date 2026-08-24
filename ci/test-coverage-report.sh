#!/usr/bin/env bash

test_coverage_report_bootstrap() {
    local report_variable=$1
    local error_prefix=$2
    local incomplete_message=$3

    command -v jq >/dev/null || {
        echo "$error_prefix error: required command 'jq' is unavailable" >&2
        exit 1
    }

    TEST_COVERAGE_REPORT_PATH=${!report_variable}
    TEST_COVERAGE_REPORT_ERROR_PREFIX=$error_prefix
    test_coverage_report_initialize \
        "$TEST_COVERAGE_REPORT_PATH" \
        "$incomplete_message"
}

test_coverage_report_initialize() {
    local report_path=$1
    local incomplete_message=$2

    mkdir -p "$(dirname "$report_path")"
    jq -n --arg error "$incomplete_message" \
        '{schemaVersion: 1, success: false, setupError: $error}' >"$report_path"
}

test_coverage_report_fail_setup() {
    local message=$1

    jq -n --arg error "$message" \
        '{schemaVersion: 1, success: false, setupError: $error}' \
        >"$TEST_COVERAGE_REPORT_PATH"
    echo "$TEST_COVERAGE_REPORT_ERROR_PREFIX error: $message" >&2
    exit 1
}
