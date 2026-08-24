#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

source ci/supported-feature-matrix.sh

readonly CRATE_NAME=lspf
readonly MANIFEST_PATH=crates/lspf/Cargo.toml
readonly APPROVALS_PATH=ci/public-api-breaking-approvals.json

report_path=target/public-api-compatibility/report.json
baseline_version=${LSPF_API_BASELINE_VERSION:-}

usage() {
    cat <<'EOF'
Usage: bash ci/check-public-api.sh [--baseline-version VERSION] [--report PATH]

Compare every supported lspf public API surface with the latest maintained
crates.io release. LSPF_API_BASELINE_VERSION overrides automatic selection.
The command always writes a JSON report after checks begin and exits nonzero
when cargo-semver-checks finds an unapproved break or cannot complete a row.
EOF
}

while (($#)); do
    case $1 in
        --baseline-version)
            baseline_version=${2:?--baseline-version requires a value}
            shift 2
            ;;
        --report)
            report_path=${2:?--report requires a value}
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "public-api error: unknown argument '$1'" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if ! command -v jq >/dev/null; then
    echo "public-api error: required command 'jq' is unavailable" >&2
    exit 1
fi

report_dir=$(dirname "$report_path")
mkdir -p "$report_dir"
jq -n '{schemaVersion: 1, success: false,
    setupError: "the compatibility gate did not complete", rows: []}' \
    >"$report_path"

fail_setup() {
    local message=$1
    jq -n --arg error "$message" \
        '{schemaVersion: 1, success: false, setupError: $error, rows: []}' \
        >"$report_path"
    echo "public-api error: $message" >&2
    exit 1
}

if ! jq -e '
    .schemaVersion == 1
    and (.approvals | type == "array")
    and all(.approvals[];
        (.baselineVersion | type == "string")
        and (.currentVersion | type == "string")
        and (.target | type == "string")
        and (.features | type == "string")
        and (.findingsSha256 | type == "string")
        and (.findingsSha256 | test("^[0-9a-f]{64}$")))
' "$APPROVALS_PATH" >/dev/null 2>&1; then
    fail_setup "invalid breaking-change approval registry: $APPROVALS_PATH"
fi

command -v cargo >/dev/null || fail_setup "required command 'cargo' is unavailable"
cargo semver-checks --version >/dev/null 2>&1 \
    || fail_setup "cargo-semver-checks is unavailable"

current_version=$(cargo metadata --no-deps --format-version 1 \
    | jq -er --arg name "$CRATE_NAME" '.packages[] | select(.name == $name) | .version')

if [[ -z $baseline_version ]]; then
    baseline_version=$(cargo info --color never "$CRATE_NAME" --registry crates-io 2>/dev/null \
        | sed -n 's/^version: //p' \
        | head -n 1)
fi
if [[ -z $baseline_version ]]; then
    fail_setup "could not select the latest crates.io baseline"
fi

IFS=. read -r baseline_major baseline_minor baseline_patch <<<"${baseline_version%%-*}"
IFS=. read -r current_major current_minor current_patch <<<"${current_version%%-*}"
if ((current_major < baseline_major)) \
    || ((current_major == baseline_major && current_minor < baseline_minor)) \
    || ((current_major == baseline_major && current_minor == baseline_minor \
        && current_patch < baseline_patch)); then
    fail_setup "current version $current_version is older than baseline $baseline_version"
fi

release_type=patch
if ((current_major > baseline_major)); then
    release_type=major
elif ((current_minor > baseline_minor)); then
    release_type=minor
fi
breaking_approvals_allowed=false
if [[ $release_type == major ]] \
    || ([[ $release_type == minor ]] \
        && ((baseline_major == 0 && current_major == 0))); then
    breaking_approvals_allowed=true
fi

rows_file=$(mktemp "$report_dir/rows.XXXXXX")
trap 'rm -f "$rows_file"' EXIT

overall_exit=0

check_surface() {
    local target=$1
    local features=$2
    local proposed=$3
    local selected_features=$features
    local surface_features=$features
    local target_name=native
    local row_output row_exit effective_exit result command_text safe_name
    local findings findings_hash approved_hash
    local -a environment=()
    local -a command=(
        cargo semver-checks check-release
        --manifest-path "$MANIFEST_PATH"
        --package "$CRATE_NAME"
        --baseline-version "$baseline_version"
        # Always ask for patch compatibility. Exact breaking-change sets may
        # be approved below only when the package version permits a break.
        --release-type patch
        --color never
        --only-explicit-features
    )

    if [[ $proposed == true ]]; then
        if [[ $selected_features == none ]]; then
            selected_features=proposed
        else
            selected_features+=,proposed
        fi
        surface_features+=,proposed
    fi
    if [[ $selected_features != none ]]; then
        command+=(--features "$selected_features")
    fi
    if [[ $target != native ]]; then
        target_name=$target
        # cargo-semver-checks cannot currently obtain wasm32 crate filenames
        # from rustc. Build rustdoc on the host while selecting the crate's
        # wasm32 cfg branches; the normal wasm jobs compile the real target.
        environment+=(
            'RUSTFLAGS=--cfg target_arch="wasm32" -Aexplicit_builtin_cfgs_in_flags'
            'RUSTDOCFLAGS=--cfg target_arch="wasm32" -Aexplicit_builtin_cfgs_in_flags'
        )
    fi

    safe_name=${target_name//[^[:alnum:]]/_}_${surface_features//[^[:alnum:]]/_}
    row_output="$report_dir/$safe_name.txt"
    printf -v command_text '%q ' env "${environment[@]}" "${command[@]}"

    set +e
    env "${environment[@]}" "${command[@]}" >"$row_output" 2>&1
    row_exit=$?
    set -e

    effective_exit=$row_exit
    findings_hash=
    case $row_exit in
        0)
            result=compatible
            ;;
        100)
            findings=$(sed -n '/^--- failure /,/^    Finished /p' "$row_output" \
                | sed -E \
                    -e '/^    Finished /d' \
                    -e 's#file /[^ ]*/lspf-'"$baseline_version"'/#file <baseline>/#g' \
                    -e 's#file [^ ]*/crates/lspf/#file <current>/#g')
            findings_hash=$(printf '%s' "$findings" | sha256sum | cut -d' ' -f1)
            approved_hash=$(jq -er \
                --arg baseline "$baseline_version" \
                --arg current "$current_version" \
                --arg target "$target_name" \
                --arg features "$surface_features" \
                '.approvals[] | select(
                    .baselineVersion == $baseline
                    and .currentVersion == $current
                    and (.target == $target or .target == "*")
                    and (.features == $features or .features == "*")
                ) | .findingsSha256' \
                "$APPROVALS_PATH" 2>/dev/null || true)
            if [[ $breaking_approvals_allowed == true \
                && $approved_hash == "$findings_hash" ]]; then
                result=approved-breaking-changes
                effective_exit=0
            else
                result=breaking-changes
            fi
            ;;
        *)
            result=error
            ;;
    esac
    if ((effective_exit != 0)); then
        overall_exit=1
    fi

    jq -cn \
        --arg target "$target_name" \
        --arg features "$surface_features" \
        --arg command "${command_text% }" \
        --arg result "$result" \
        --arg findingsSha256 "$findings_hash" \
        --argjson toolExitCode "$row_exit" \
        --argjson exitCode "$effective_exit" \
        --rawfile output "$row_output" \
        '{target: $target, features: $features, command: $command,
          result: $result, findingsSha256:
            (if $findingsSha256 == "" then null else $findingsSha256 end),
          toolExitCode: $toolExitCode, exitCode: $exitCode, output: $output}' \
        >>"$rows_file"

    printf '%-28s %s\n' "$target_name/$surface_features" "$result"
}

for features in "${NATIVE_FEATURE_SELECTIONS[@]}"; do
    check_surface native "$features" false
    check_surface native "$features" true
done
for features in "${WASM_FEATURE_SELECTIONS[@]}"; do
    check_surface wasm32-unknown-unknown "$features" false
    check_surface wasm32-unknown-unknown "$features" true
done

jq -s \
    --arg crate "$CRATE_NAME" \
    --arg baselineVersion "$baseline_version" \
    --arg currentVersion "$current_version" \
    --arg releaseType "$release_type" \
    '{schemaVersion: 1, crate: $crate, baselineVersion: $baselineVersion,
      currentVersion: $currentVersion, releaseType: $releaseType,
      intentionalPre1BreakingChanges:
        (any(.[]; .result == "approved-breaking-changes")
          and ($baselineVersion | startswith("0."))
          and ($releaseType == "minor")),
      success: (all(.[]; .exitCode == 0)), rows: .}' \
    "$rows_file" >"$report_path"

echo "Public API compatibility report: $report_path"
exit "$overall_exit"
