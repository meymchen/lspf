#!/usr/bin/env bash

set -euo pipefail

readonly TARGETS=(
    "envelope 65536 5 300"
    "content_length 65536 5 300"
    "uri_identity 4096 5 300"
    "position_conversion 65536 5 300"
    "incremental_edits 65536 5 300"
    "lifecycle_sequences 16384 10 300"
)

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
readonly cargo_bin=${FUZZ_CARGO_BIN:-cargo}
readonly artifact_root=${FUZZ_ARTIFACT_ROOT:-$repo_root/fuzz/artifacts}

check_contract() {
    local config target max_len timeout budget
    local manifest="$repo_root/fuzz/Cargo.toml"
    local guide="$repo_root/fuzz/README.md"
    local workflow="$repo_root/.github/workflows/fuzz.yml"

    for required in "$manifest" "$guide" "$workflow"; do
        if [[ ! -f $required ]]; then
            echo "missing fuzz contract file: ${required#"$repo_root/"}" >&2
            return 1
        fi
    done

    for config in "${TARGETS[@]}"; do
        read -r target max_len timeout budget <<<"$config"
        if [[ ! -f "$repo_root/fuzz/fuzz_targets/$target.rs" ]]; then
            echo "missing fuzz target source: $target" >&2
            return 1
        fi
        if ! grep -q "name = \"$target\"" "$manifest"; then
            echo "fuzz target is absent from fuzz/Cargo.toml: $target" >&2
            return 1
        fi
        if ! compgen -G "$repo_root/fuzz/corpus/$target/valid-*" >/dev/null; then
            echo "fuzz target has no valid seed: $target" >&2
            return 1
        fi
        if ! compgen -G "$repo_root/fuzz/corpus/$target/malformed-*" >/dev/null; then
            echo "fuzz target has no malformed seed: $target" >&2
            return 1
        fi
        local expected_row="| \`$target\` | $max_len | $timeout s | $budget s |"
        if ! grep -Fqx "$expected_row" "$guide"; then
            echo "fuzz target limits are undocumented or stale: $target" >&2
            return 1
        fi
    done

    # The scheduled workflow may sweep every target directly, or reach it
    # through Gate D verification, which runs the same `--all` sweep as one of
    # its components. Follow the second hop rather than accepting the mere
    # mention of the Gate D script, so this keeps proving that a scheduled run
    # covers every target.
    local gate_d="$repo_root/ci/run-gate-d-evidence.sh"
    if ! grep -q 'bash ci/run-fuzz.sh --all' "$workflow" \
        && ! { grep -q 'bash ci/run-gate-d-evidence.sh' "$workflow" \
            && grep -q 'bash ci/run-fuzz.sh --all' "$gate_d"; }
    then
        echo "scheduled workflow does not run every fuzz target" >&2
        return 1
    fi
}

run_target() {
    local target=$1
    local max_len=$2
    local timeout=$3
    local budget=$4
    local artifact_dir="$artifact_root/$target"
    local corpus_dir="$repo_root/fuzz/corpus/$target"
    local marker
    local status=0

    mkdir -p "$artifact_dir"
    marker=$(mktemp "$artifact_dir/.run.XXXXXX")
    "$cargo_bin" +nightly fuzz run "$target" "$corpus_dir" -- \
        "-max_len=$max_len" \
        "-timeout=$timeout" \
        "-max_total_time=$budget" \
        "-artifact_prefix=$artifact_dir/" || status=$?

    if (( status == 0 )); then
        rm -f "$marker"
        return 0
    fi

    local artifact
    artifact=$(find "$artifact_dir" -maxdepth 1 -type f ! -name '*.minimized' \
        ! -name '.run.*' -newer "$marker" -printf '%T@ %p\n' \
        | sort -nr | sed -n '1s/^[^ ]* //p')
    rm -f "$marker"
    if [[ -n $artifact ]]; then
        "$cargo_bin" +nightly fuzz tmin "$target" "$artifact" -- \
            "-timeout=$timeout" \
            "-exact_artifact_path=$artifact.minimized" || true
    else
        echo "fuzz target failed without writing a reproducer: $target" >&2
    fi
    return "$status"
}

if [[ ${1:-} == "--list" ]]; then
    printf '%s\n' "${TARGETS[@]}"
    exit 0
fi

if [[ ${1:-} == "--check" ]]; then
    check_contract
    exit 0
fi

if [[ ${1:-} == "--all" ]]; then
    check_contract
    failures=0
    for config in "${TARGETS[@]}"; do
        read -r target max_len timeout budget <<<"$config"
        if ! run_target "$target" "$max_len" "$timeout" "$budget"; then
            failures=$((failures + 1))
        fi
    done
    if (( failures > 0 )); then
        echo "$failures fuzz target(s) failed" >&2
        exit 1
    fi
    exit 0
fi

echo "usage: $0 --list | --check | --all" >&2
exit 2
