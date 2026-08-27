#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ci_workflow="$repo_root/.github/workflows/ci.yml"
yq_bin="${YQ_BIN:-yq}"

yaml_to_json() {
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

yaml_to_json "$ci_workflow" | jq -e '
  .concurrency.group
    == "${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}"
  and .concurrency["cancel-in-progress"]
    == "${{ github.event_name == '\''pull_request'\'' }}"
' >/dev/null

ci_json="$(yaml_to_json "$ci_workflow")"
for job in feature-matrix msrv native-matrix test-coverage gate-b-evidence gate-c-evidence
do
    jq -e --arg job "$job" '
      .jobs[$job].if == "${{ github.event_name == '\''push'\'' }}"
    ' <<<"$ci_json" >/dev/null
done

echo 'CI pull-request fast-path contract verified'
