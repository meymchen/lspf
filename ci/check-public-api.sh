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

# The manifest version is deliberately not consulted beyond that sanity check.
# release-plz owns the bump: it opens the release pull request that raises the
# version and runs the same pinned cargo-semver-checks to choose major, minor,
# or patch. A feature branch therefore still carries the published version
# while it introduces the break, so requiring a bump here would force every
# breaking pull request to hand-edit a number that release-plz is about to
# compute. An approval records the reviewed findings instead.

rows_file=$(mktemp "$report_dir/rows.XXXXXX")
trap 'rm -f "$rows_file"' EXIT

overall_exit=0

check_surface() {
    local target=$1
    local features=$2
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

    if [[ $selected_features != none ]]; then
        command+=(--features "$selected_features")
    fi
    if [[ $target != native ]]; then
        target_name=$target
        # cargo-semver-checks cannot currently obtain wasm32 crate filenames
        # from rustc. Let host rustdoc select the crate's wasm32 cfg branches,
        # but do not inject the synthetic cfg through RUSTFLAGS: Cargo would
        # then also compile dependencies as if the host were wasm32. The normal
        # wasm jobs compile the real target.
        environment+=(
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
                | tr -d '\r' \
                | tr '\\' '/' \
                | sed -E \
                    -e '/^    Finished /d' \
                    -e 's#[^[:space:]]*/lspf-'"$baseline_version"'/#<baseline>/#g' \
                    -e 's#[^[:space:]]*/crates/lspf/#<current>/#g')
            # cargo-semver-checks does not guarantee finding or item ordering.
            # Sort lines inside each failure block, then sort the blocks, so
            # equivalent findings have one fingerprint without losing which
            # lint reported each item.
            canonical_findings=$(printf '%s\n' "$findings" \
                | jq -Rrs '
                    [splits("(?m)(?=^--- failure )")
                     | select(length > 0)
                     | split("\n") | sort | join("\n")]
                    | sort | join("\n")
                ' \
                | tr -d '\r')
            findings_hash=$(printf '%s' "$canonical_findings" \
                | sha256sum \
                | cut -d' ' -f1)
            # Match on the fingerprint rather than reading one out and
            # comparing: successive breaking changes accumulate entries under
            # the same baseline, so more than one row can match the surface.
            if jq -e \
                --arg baseline "$baseline_version" \
                --arg target "$target_name" \
                --arg features "$surface_features" \
                --arg hash "$findings_hash" \
                'any(.approvals[];
                    .baselineVersion == $baseline
                    and (.target == $target or .target == "*")
                    and (.features == $features or .features == "*")
                    and .findingsSha256 == $hash)' \
                "$APPROVALS_PATH" >/dev/null 2>&1; then
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

for surface in "${PUBLIC_API_SURFACES[@]}"; do
    IFS='|' read -r target features <<<"$surface"
    check_surface "$target" "$features"
done

jq -s \
    --arg crate "$CRATE_NAME" \
    --arg baselineVersion "$baseline_version" \
    --arg currentVersion "$current_version" \
    '{schemaVersion: 1, crate: $crate, baselineVersion: $baselineVersion,
      currentVersion: $currentVersion,
      intentionalPre1BreakingChanges:
        (any(.[]; .result == "approved-breaking-changes")
          and ($baselineVersion | startswith("0."))),
      success: (all(.[]; .exitCode == 0)), rows: .}' \
    "$rows_file" >"$report_path"

echo "Public API compatibility report: $report_path"
if ((overall_exit != 0)); then
    if jq -s -e 'any(.[]; .result == "breaking-changes")' "$rows_file" >/dev/null
    then
        echo "public-api error: unapproved breaking changes; add the candidate below to $APPROVALS_PATH to record the break as reviewed. release-plz picks the version bump from the same findings, so do not edit the manifest version." >&2
    fi
    jq -rs --arg baseline "$baseline_version" '
        [ .[] | select(.result == "breaking-changes") ] as $breaks
        | if ($breaks | length) > 0
            and ([$breaks[].findingsSha256] | unique | length) == 1
          then [{baselineVersion: $baseline,
                 target: "*", features: "*",
                 findingsSha256: $breaks[0].findingsSha256}]
          else [$breaks[]
                | {baselineVersion: $baseline,
                   target, features, findingsSha256}]
          end
        | unique
        | .[]
        | "approval candidate: " + (@json)
    ' "$rows_file" >&2
fi
exit "$overall_exit"
