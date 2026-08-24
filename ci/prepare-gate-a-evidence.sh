#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

revision="${1:?usage: prepare-gate-a-evidence.sh REVISION RUN_URL OUTPUT_DIRECTORY}"
run_url="${2:?usage: prepare-gate-a-evidence.sh REVISION RUN_URL OUTPUT_DIRECTORY}"
output_dir="${3:?usage: prepare-gate-a-evidence.sh REVISION RUN_URL OUTPUT_DIRECTORY}"
repository="${GITHUB_REPOSITORY:-meymchen/lspf}"
server_url="${GITHUB_SERVER_URL:-https://github.com}"
job_results="${GATE_A_JOB_RESULTS:?GATE_A_JOB_RESULTS must contain the CI needs context as JSON}"

if [[ ! $revision =~ ^[0-9a-f]{40}$ ]]; then
    printf 'Gate A evidence revision must be a full lowercase commit SHA: %s\n' \
        "$revision" >&2
    exit 1
fi
if [[ ! $run_url =~ ^https:// ]]; then
    printf 'Gate A evidence run URL must use HTTPS: %s\n' "$run_url" >&2
    exit 1
fi
if [[ -e $output_dir ]]; then
    printf 'Gate A evidence output already exists: %s\n' "$output_dir" >&2
    exit 1
fi
if ! jq -e 'type == "object"' <<<"$job_results" >/dev/null 2>&1; then
    echo 'GATE_A_JOB_RESULTS must be a JSON object' >&2
    exit 1
fi

mkdir -p "$output_dir"

jq -n \
    --arg revision "$revision" \
    --arg run "$run_url" \
    --arg repository "$repository" \
    --arg server "$server_url" \
    --argjson results "$job_results" '
    def source($path):
      ($server + "/" + $repository + "/blob/" + $revision + "/" + $path);
    def check($key; $name):
      {id: $key, name: $name, result: ($results[$key].result // "missing"), run: $run};
    def artifact($name; $description):
      {name: $name, description: $description, run: $run};
    {
      schemaVersion: 1,
      gate: "A",
      revision: $revision,
      sourceRepository: ($server + "/" + $repository),
      workflowRun: $run,
      claims: [
        {
          id: "support-matrix",
          statement: "Every declared Rust version, host, target, and Cargo feature selection has an automated enforcement gate.",
          classification: "automated",
          sources: [source("SECURITY.md"), source("ci/native-feature-matrix.json"), source("ci/check-feature-contract.sh")],
          checks: [
            check("msrv"; "MSRV matrix"),
            check("feature-contract"; "Feature and target contract"),
            check("native-matrix"; "Native feature matrix"),
            check("test"; "Workspace tests"),
            check("native-lifecycle"; "Cross-platform native lifecycle"),
            check("wasm"; "WASM build and tests")
          ]
        },
        {
          id: "documentation",
          statement: "Public documentation builds for the supported surfaces with warnings denied, and repository Markdown is linted.",
          classification: "automated",
          sources: [source("ci/check-public-docs.sh"), source(".github/workflows/public-docs.yml")],
          checks: [check("public-docs"; "Public documentation"), check("markdownlint"; "Markdown lint")]
        },
        {
          id: "compatibility-policy",
          statement: "The compatibility policy is documented and every supported public API surface is compared with its released baseline.",
          classification: "automated",
          sources: [source("SECURITY.md"), source("ci/check-public-api.sh"), source("ci/public-api-breaking-approvals.json")],
          checks: [check("public-api"; "Public API compatibility")]
        },
        {
          id: "packaged-consumer",
          statement: "The publishable crate contents, external consumer lifecycle, and package-only documentation are verified.",
          classification: "automated",
          sources: [source("ci/check-packaged-crate.sh"), source(".github/workflows/packaged-crate.yml")],
          checks: [check("packaged-crate"; "Packaged crate consumer")]
        },
        {
          id: "coverage",
          statement: "Workspace coverage and protocol-engine coverage are checked against recorded baselines with named failure-path tests.",
          classification: "automated",
          sources: [source("ci/check-test-coverage.sh"), source("ci/test-coverage-baseline.json"), source("ci/run-coverage-evidence-tests.sh")],
          checks: [check("test-coverage"; "Test coverage")]
        },
        {
          id: "supply-chain",
          statement: "Dependency advisories, licenses, Action pins, and workflow permissions are automatically audited.",
          classification: "automated",
          sources: [source(".github/workflows/security.yml"), source("deny.toml"), source("ci/check-workflow-security.sh")],
          checks: [check("security"; "Supply-chain security")]
        },
        {
          id: "release-traceability",
          statement: "The release workflow contract retains package metadata, hashes, provenance, and an SBOM from one revision.",
          classification: "automated",
          sources: [source("ci/prepare-release-artifacts.sh"), source("ci/test-release-artifacts-workflow.sh"), source(".github/workflows/release-plz.yml")],
          checks: [check("security"; "Release artifact workflow contract")]
        }
      ],
      artifacts: [
        artifact("public-api-compatibility-report"; "Machine-readable compatibility results for each supported public API surface."),
        artifact("test-coverage-report"; "HTML, LCOV, raw summary, baseline summary, and named failure-path evidence."),
        artifact("gate-a-release-evidence"; "This revision-locked JSON and Markdown evidence bundle.")
      ],
      humanJudgments: [
        {
          statement: "The support promise, response timelines, and the final decision that Gate A is satisfied remain maintainer commitments and judgments.",
          classification: "human",
          sources: [source("SECURITY.md")]
        },
        {
          statement: "Whether an intentional breaking change and its release notes are acceptable requires maintainer review; automation only matches the recorded approval to exact findings.",
          classification: "human",
          sources: [source("SECURITY.md"), source("ci/public-api-breaking-approvals.json")]
        },
        {
          statement: "Vulnerability severity, project-code security, disclosure timing, and reporter communication require human security review; supply-chain checks do not establish their absence.",
          classification: "human",
          sources: [source("SECURITY.md")]
        }
      ]
    }
    | .failedChecks = ([
        .claims[].checks[]
        | select(.result != "success")
      ] | unique_by(.id) | map(
        . + {explanation: (
          if .result == "failure" then
            "The CI job failed; inspect the linked run for its command output and retained artifacts."
          elif .result == "cancelled" then
            "The CI job was cancelled before it could establish the claim; rerun the complete gate."
          elif .result == "skipped" then
            "The CI job was skipped, so this revision has no passing evidence for the claim."
          else
            "The CI job did not report success; inspect the linked run before accepting Gate A."
          end
        )}
      ))
    | .overallResult = (
        if (.failedChecks | length) == 0 then "success" else "failure" end
      )
  ' >"$output_dir/evidence.json"

run_number="${run_url##*/}"
jq -r --arg run_number "$run_number" '
  "# Gate A release evidence\n",
  "Revision: [" + .revision + "](" + .sourceRepository + "/commit/" + .revision + ")",
  (if .overallResult == "success" then "Passing run: " else "Workflow run: " end)
    + "[CI run " + $run_number + "](" + .workflowRun + ")",
  "Overall result: **" + .overallResult + "**\n",
  "## Automated claims\n",
  (.claims[] |
    "### " + .statement + "\n",
    "Classification: `" + .classification + "`\n",
    "Implementation:\n",
    (.sources[] | "- [Revision-locked source](" + . + ")"),
    "\nChecks:\n",
    (.checks[] | "- " + .name + ": `" + .result + "` ([run](" + .run + "))"),
    ""),
  "## Recorded artifacts\n",
  (.artifacts[] | "- `" + .name + "`: " + .description + " ([run](" + .run + "))"),
  (if (.failedChecks | length) > 0 then
    "\n## Failing checks\n",
    (.failedChecks[] |
      "- " + .name + ": `" + .result + "`. " + .explanation
        + " ([run](" + .run + "))")
  else empty end),
  "\n## Human judgments\n",
  "These items are deliberately not presented as automated proof.\n",
  (.humanJudgments[] |
    "- " + .statement + " Classification: `" + .classification + "`. "
      + ([.sources[] | "[policy](" + . + ")"] | join(" ")))
' "$output_dir/evidence.json" >"$output_dir/evidence.md"

echo "Gate A release evidence prepared for $revision"

if ! jq -e '.overallResult == "success" and (.failedChecks | length) == 0' \
    "$output_dir/evidence.json" >/dev/null
then
    echo 'Gate A release evidence contains checks without a passing result' >&2
    exit 1
fi
