#!/usr/bin/env bash

set -euo pipefail

readonly TARGETS=(
    "envelope 65536 5 300"
    "content-length 65536 5 300"
    "uri-identity 4096 5 300"
    "position-conversion 65536 5 300"
    "incremental-edits 65536 5 300"
    "notebook-cell-sync 4096 5 300"
    "lifecycle-sequences 16384 10 300"
)

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
readonly cargo_bin=${FUZZ_CARGO_BIN:-cargo}
# Not readonly: `--target` redirects reproducers into the result directory that
# leg hands to the aggregating job.
artifact_root=${FUZZ_ARTIFACT_ROOT:-$repo_root/fuzz/artifacts}

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

    # The scheduled workflow may sweep every target directly, reach it through
    # Gate D verification, which runs the same `--all` sweep as one of its
    # components, or fan the targets out as a matrix that Gate D reassembles.
    # Follow the second hop rather than accepting the mere mention of the Gate D
    # script, so this keeps proving that a scheduled run covers every target.
    # The matrix form counts because `--matrix` and `--collect` read the same
    # list checked above, and `--collect` fails on a target that never reported.
    # The branches are ordered by what the workflow itself does, so the shape it
    # actually takes is the one that has to hold up.
    local gate_d="$repo_root/ci/run-gate-d-evidence.sh"
    local swept=1
    if grep -q 'bash ci/run-fuzz.sh --all' "$workflow"; then
        swept=0
    elif grep -q 'ci/run-fuzz.sh --matrix' "$workflow" \
        && grep -q 'ci/run-fuzz.sh --target' "$workflow"; then
        if grep -q 'bash ci/run-gate-d-evidence.sh' "$workflow" \
            && grep -q 'bash ci/run-fuzz.sh --collect' "$gate_d"; then
            swept=0
        fi
    elif grep -q 'bash ci/run-gate-d-evidence.sh' "$workflow" \
        && grep -q 'bash ci/run-fuzz.sh --all' "$gate_d"; then
        swept=0
    fi
    if ((swept != 0)); then
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

target_budgets() {
    local wanted=$1
    local config target max_len timeout budget

    for config in "${TARGETS[@]}"; do
        read -r target max_len timeout budget <<<"$config"
        if [[ $target == "$wanted" ]]; then
            printf '%s %s %s\n' "$max_len" "$timeout" "$budget"
            return 0
        fi
    done
    return 1
}

# One leg of a parallel sweep. Every target still runs against the same budgets
# `--all` uses; what changes is that the leg leaves its output, exit status,
# duration and reproducers behind as files, so a later job can reassemble the
# sweep from parts that ran on separate machines.
run_single_target() {
    local target=$1
    local result_dir=$2
    local budgets max_len timeout budget
    local status=0 started_ns finished_ns

    if ! budgets=$(target_budgets "$target"); then
        echo "unknown fuzz target: $target" >&2
        exit 2
    fi
    read -r max_len timeout budget <<<"$budgets"

    check_contract
    mkdir -p "$result_dir"
    artifact_root="$result_dir/reproducers"

    started_ns=$(date +%s%N)
    set +e
    run_target "$target" "$max_len" "$timeout" "$budget" 2>&1 \
        | tee "$result_dir/log"
    status=${PIPESTATUS[0]}
    set -e
    finished_ns=$(date +%s%N)

    printf '%s\n' "$status" >"$result_dir/status"
    printf '%s\n' "$(((finished_ns - started_ns) / 1000000))" \
        >"$result_dir/duration-ms"
    return "$status"
}

# The aggregating job's half of the same sweep. Replaying each leg's output
# keeps the Gate D component log holding what a serial `--all` would have
# produced, and a leg that never reported is a failure rather than a silent
# gap: evidence that a target ran is the point of the component.
collect_results() {
    local result_root=$1
    local component_dir=$2
    local config target max_len timeout budget
    local leg status duration
    local missing=0 failures=0 longest=0 total=0

    check_contract
    mkdir -p "$component_dir"

    for config in "${TARGETS[@]}"; do
        read -r target max_len timeout budget <<<"$config"
        leg="$result_root/$target"
        if [[ ! -f "$leg/status" || ! -f "$leg/log" ]]; then
            echo "fuzz target reported no result: $target" >&2
            missing=$((missing + 1))
            continue
        fi

        status=$(cat "$leg/status")
        duration=$(cat "$leg/duration-ms" 2>/dev/null || echo 0)
        printf '===== %s: exit %s in %s ms =====\n' "$target" "$status" "$duration"
        cat "$leg/log"
        if ((duration > longest)); then
            longest=$duration
        fi
        total=$((total + duration))
        if ((status != 0)); then
            failures=$((failures + 1))
        fi
        if [[ -d "$leg/reproducers" ]]; then
            mkdir -p "$component_dir/reproducers"
            cp -R "$leg/reproducers/." "$component_dir/reproducers/"
        fi
    done

    # The component's recorded duration is the sweep's wall clock, not the time
    # this job spent collecting it, so the evidence keeps meaning what it meant
    # when every target ran here in sequence.
    printf '%s\n' "$longest" >"$component_dir/duration-ms"
    printf 'Fuzzed %s targets in parallel: %s ms of fuzzing, %s ms wall clock\n' \
        "${#TARGETS[@]}" "$total" "$longest"

    if ((missing > 0 || failures > 0)); then
        echo "$failures fuzz target(s) failed, $missing did not report" >&2
        return 1
    fi
}

if [[ ${1:-} == "--list" ]]; then
    printf '%s\n' "${TARGETS[@]}"
    exit 0
fi

if [[ ${1:-} == "--matrix" ]]; then
    printf '%s\n' "${TARGETS[@]}" | awk '{print $1}' \
        | jq -Rsc 'split("\n") | map(select(length > 0)) | {target: .}'
    exit 0
fi

if [[ ${1:-} == "--check" ]]; then
    check_contract
    exit 0
fi

if [[ ${1:-} == "--target" ]]; then
    if (($# != 3)); then
        echo "usage: $0 --target NAME RESULT_DIRECTORY" >&2
        exit 2
    fi
    target_status=0
    run_single_target "$2" "$3" || target_status=$?
    exit "$target_status"
fi

if [[ ${1:-} == "--collect" ]]; then
    if (($# != 3)); then
        echo "usage: $0 --collect RESULT_DIRECTORY COMPONENT_DIRECTORY" >&2
        exit 2
    fi
    collect_status=0
    collect_results "$2" "$3" || collect_status=$?
    exit "$collect_status"
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

cat >&2 <<EOF
usage: $0 --list
       $0 --matrix
       $0 --check
       $0 --all
       $0 --target NAME RESULT_DIRECTORY
       $0 --collect RESULT_DIRECTORY COMPONENT_DIRECTORY
EOF
exit 2
