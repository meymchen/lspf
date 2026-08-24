#!/usr/bin/env bash

test_coverage_baseline_is_valid() {
    local baseline_path=$1
    local validation_mode=${2:-full}

    jq -e --arg validationMode "$validation_mode" '
        def valid_evidence:
          . as $evidence
          | type == "object"
          and (["lifecycle", "cancellation", "malformedMessage", "close"]
            | all(.[];
                ($evidence[.] | type == "array" and length > 0)
                and all($evidence[.][];
                  type == "object"
                  and (keys | sort == ["name", "target"])
                  and (.target | type == "string"
                    and test("^[A-Za-z0-9_]+$"))
                  and (.name | type == "string"
                    and test("^[A-Za-z0-9_]+(::[A-Za-z0-9_]+)*$")))));

        . as $root
        | ($validationMode == "full" or $validationMode == "evidence")
        and .schemaVersion == 1
        and ($root.evidence | valid_evidence)
        and ($validationMode == "evidence" or (
          all(.thresholds.workspaceLines, .thresholds.protocolEngineLines;
            . as $threshold
            | ($threshold.count | type == "number" and . > 0)
            and ($threshold.covered | type == "number" and . >= 0)
            and ($threshold.covered <= $threshold.count))
          and (.protocolEngineFiles | type == "array" and length > 0)
          and all(.protocolEngineFiles[]; type == "string" and length > 0)
        ))
    ' "$baseline_path" >/dev/null 2>&1
}
