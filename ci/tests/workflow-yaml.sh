#!/usr/bin/env bash

workflow_yaml_to_json() {
    local yq_bin="${YQ_BIN:-yq}"

    if command -v "$yq_bin" >/dev/null; then
        "$yq_bin" -o=json '.' "$1"
    else
        python3 -c '
import json
import sys

import yaml

with open(sys.argv[1], encoding="utf-8") as source:
    json.dump(yaml.safe_load(source), sys.stdout)
' "$1"
    fi
}
