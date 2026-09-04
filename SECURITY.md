# Support, compatibility, and security policy

This document is the support contract for released versions of `lspf`. It
defines which Rust versions, hosts, targets, and Cargo feature selections the
maintainers support, as well as the rules for compatibility, deprecation, and
security reports.

## Release support window

`lspf` does not publish long-term-support releases. The latest patch release
of the latest minor line is the only maintained release. For example, after
`0.6.0` is published, the `0.5.x` line is no longer maintained. Users must
reproduce a bug on the maintained line before the maintainers commit to a
fix.

Security advisories identify every known affected release, but fixes target
the maintained line. The maintainers may backport a low-risk security fix,
but this is not part of the support promise.

## Rust versions

The minimum supported Rust version (MSRV) is **1.98.0**. The supported compiler
range is Rust 1.98.0 through the latest stable Rust release. Nightly and beta
toolchains are useful for early warning, but are not supported toolchains.

The workspace `rust-version` field is the source of truth. Development and all
Rust CI jobs pin Rust 1.98.0 through `rust-toolchain.toml` and the shared setup
action. On pull requests, CI `feature-contract` compiles every documented
feature selection for the Linux host and `wasm32-unknown-unknown`. After a
change reaches `main`, CI `msrv` also checks the release-oriented native matrix
and the whole workspace with default features on the same compiler.

An MSRV increase is a breaking change. Before 1.0 it may happen only in a new
minor release; after 1.0 it requires a new major release. The release notes
must name the old and new MSRV. A patch release must not raise the MSRV.

## Supported hosts and targets

The following table is exhaustive. "Supported" means maintainers accept bug
reports and intend to fix regressions on the maintained release line.

| Host | Rust target | Status | Enforcement gate |
| --- | --- | --- | --- |
| Linux | `x86_64-unknown-linux-gnu` | Supported | CI `native-matrix`, `test`, and `native-lifecycle` |
| Windows | `x86_64-pc-windows-msvc` | Supported | CI `native-lifecycle` |
| macOS | `x86_64-apple-darwin` | Supported | CI `native-lifecycle` |
| macOS | `aarch64-apple-darwin` | Supported | CI `native-lifecycle` |
| Browser or Node Worker | `wasm32-unknown-unknown` | Supported | CI `wasm` and `feature-contract` |

CI `native-lifecycle` runs the same default-feature stdio journey on Linux,
Windows, and macOS. Other operating systems, architectures, WASI targets, and
embedded or `no_std` environments are unsupported. Reports with a small,
portable fix are welcome, but accepting such a fix does not add the host or
target to this matrix.

## Cargo feature contract

Default features select `stdio`. The rows below cover every supported feature
selection. Features in the same supported native row may be combined, and
`testing` may be added to any native row. No other cross-target combination is
supported.

| Target family | Feature selection | Status | Enforcement gate |
| --- | --- | --- | --- |
| Native | default features or `stdio` | Supported stdio Transport | CI `msrv`, `native-matrix`, and `test` |
| Native | `tcp` | Supported TCP Transport | CI `msrv`, `native-matrix`, and `test` |
| Native | `websocket` | Supported WebSocket Transport | CI `msrv`, `native-matrix`, and `test` |
| Native | any combination of `stdio`, `tcp`, and `websocket` | Supported | CI `msrv` row `all-native-features` and CI `test` |
| Native | `runtime-tokio` without a first-party adapter | Supported for a custom Transport | CI `feature-contract` |
| Native | `testing` alone or added to another native row | Supported in-memory test Transport, protocol journeys, and virtual clock; implies `runtime-tokio` | CI `feature-contract`, `native-matrix`, and `test` |
| Native | no runtime or Transport feature | Supported for protocol-only compilation, not serving | CI `feature-contract` |
| Native | `fuzzing` | Unsupported; this repository's own fuzz-harness surface | CI `fuzz contract` |
| Native | `worker-channel` | Intentionally invalid | CI `feature-contract` checks the compile-time diagnostic |
| `wasm32-unknown-unknown` | `wasm` without a first-party adapter | Supported for a custom Transport | CI `wasm` |
| `wasm32-unknown-unknown` | `worker-channel` | Supported MessagePort Transport; implies `wasm` | CI `wasm` and `feature-contract` |
| `wasm32-unknown-unknown` | no `wasm` | Intentionally invalid | CI `feature-contract` checks the compile-time diagnostic |
| `wasm32-unknown-unknown` | default features or `stdio` | Unsupported | Not applicable; this selection is outside the support contract |
| `wasm32-unknown-unknown` | `tcp` or `websocket` | Intentionally invalid | CI `feature-contract` checks the compile-time diagnostics |
| `wasm32-unknown-unknown` | `testing` | Intentionally invalid | CI `feature-contract` checks the compile-time diagnostic |

The [Transport guide](https://meymchen.github.io/lspf/en/guides/transports/)
gives build commands and
describes the APIs enabled by each feature. CI `feature-contract` compiles
every supported selection and checks that Transport-specific dependencies do
not leak into the default build.

`fuzzing` enables `lspf::fuzzing`, the harness the repository's own cargo-fuzz
package drives. It is hidden from the crate documentation, carries no
compatibility promise, and follows the fuzz targets. Downstream code should not
enable it; the supported downstream test surface is `testing`.

Pull requests use a fast feedback path: `feature-contract`, maximal workspace
tests, public documentation, packaging, compatibility, security, WASM, and the
cross-platform lifecycle run before merge. The release-oriented `msrv` and
native test matrices, coverage, and Gate A/B evidence run after a push to
`main`. A newer commit on the same pull request cancels its older CI run.

## The frozen public interface

[`docs/public-interface.md`](./docs/public-interface.md) enumerates the public
interface item by item: every crate-root export and the target and feature
selection it is available under, the `lspf::testing` surface, the type aliases
`lspf::types` owns, the crates whose types appear in frozen signatures, what
the crate exposes without freezing, and the capabilities this release
deliberately defers. CI `public-interface` compares that inventory with the
crate in both directions, so an export nobody inventoried and an inventory row
whose export is gone both fail before merge.

## Semantic versioning

Releases follow Cargo's interpretation of semantic versioning. The public Rust
API, Cargo feature names, default feature set, documented behavior, and
supported target matrix are part of the compatibility contract.

While the crate is below 1.0:

- patch releases preserve compatibility;
- a minor release may contain breaking changes, which must be called out in
  the changelog and release notes.

After 1.0, breaking changes require a major release. Adding an optional API or
loosening an input requirement is normally compatible. Changing a default
feature, removing a feature, tightening a public bound, or dropping a
supported target is breaking.

CI `public API compatibility` compares the crate with the latest version on
crates.io, which is the maintained baseline. The exhaustive `feature-contract`
and public-docs gates own combination correctness. Compatibility checks the
core and additive maximal API surfaces for native and WASM targets: native
without features, native with `stdio,tcp,websocket,testing`, WASM with `wasm`,
and WASM with `worker-channel`. The job uploads
`public-api-compatibility-report`, a JSON artifact containing the baseline,
current version, command output, result, and exit code for every surface.

For the WASM rows, rustdoc runs on the CI host with the crate's
`target_arch = "wasm32"` branches selected. This works around a
cargo-semver-checks target-metadata limitation while still comparing the
WASM-only Rust interface. The separate CI `wasm` and `feature-contract` jobs
compile those selections for the real `wasm32-unknown-unknown` target.

The gate always asks cargo-semver-checks to apply patch-level compatibility.
Exit code 100 means the tool found a break. Each failing report row includes a
hash of normalized, order-independent failure blocks, so output ordering cannot
split one finding set into several approvals. An intentional break can
be approved in `ci/public-api-breaking-approvals.json` for one baseline,
current version, target, and feature selection. A different or additional
finding changes the hash and still fails the gate.

To approve a reviewed break, copy the gate's `approval candidate` JSON into the
`approvals` array. When every representative surface has the same findings,
the candidate uses `"target": "*"` and `"features": "*"`; otherwise the gate
prints the exact surface-specific records. Rerun the gate and commit the
approval with the breaking change. Remove approval entries after their
`currentVersion` is released; a later baseline or version cannot reuse them.

An approval is valid only for a pre-1.0 minor version bump or a post-1.0 major
version bump. The changelog and release notes must describe every approved
break. An unchanged version, a patch version, and a post-1.0 minor version
reject approval records. Any other nonzero tool exit means a surface could not
be checked. Both tool errors and unapproved findings fail the CI job after the
report is written. Setup failures write a JSON error report before exiting.

## Deprecation

Stable public API is deprecated before removal. Before 1.0, it remains
available for at least one complete minor release and may be removed in the
following minor release. After 1.0, it remains available until the next major
release. The deprecation notice and changelog must name the replacement or
explain why none exists.

The maintainers may bypass the normal deprecation window when an API is
unsound, enables a vulnerability, or cannot work as documented. The release
notes must explain the exception and provide migration instructions when a
safe replacement exists.

The public API compatibility gate detects removal of deprecated and
non-deprecated API. CI `markdownlint` checks this policy and the other
Markdown files. CI `public docs` checks warning-free documentation for the
same target and feature surfaces.

## Gate A release evidence

After a push to `main`, CI `gate-a-evidence` waits for the support matrix,
documentation, compatibility, packaged consumer, coverage, and supply-chain
jobs. It writes
`gate-a-release-evidence`, which contains a JSON manifest and a Markdown
summary. Both files identify the full commit SHA and workflow run. Source
links use that SHA rather than a moving branch, and the manifest records the
other machine-readable artifacts by name.

The job runs even when one of its dependencies fails or is skipped. In that
case, the evidence files list each result and explain why the claim is not
established. The assembler then exits unsuccessfully, while the upload step
still retains the files for diagnosis.

Download an evidence artifact and reproduce its generated files with:

```bash
gh run download RUN_ID \
  --name gate-a-release-evidence \
  --dir target/downloaded-gate-a-evidence
revision="$(jq -r .revision target/downloaded-gate-a-evidence/evidence.json)"
run_url="$(jq -r .workflowRun target/downloaded-gate-a-evidence/evidence.json)"
job_results="$(jq '
  [.claims[].checks[]]
  | unique_by(.id)
  | map({key: .id, value: {result: .result}})
  | from_entries
' target/downloaded-gate-a-evidence/evidence.json)"
GATE_A_JOB_RESULTS="$job_results" \
  bash ci/prepare-gate-a-evidence.sh \
    "$revision" "$run_url" target/reproduced-gate-a-evidence
```

The manifest labels maintainer commitments and reviews as human judgments.
For example, CI can verify the support policy exists and that its named gates
passed. It cannot prove that future support responses will meet the policy,
approve the substance of an intentional breaking change, or assess a private
vulnerability report.

## Gate E candidate validation

Gates A through D establish properties of a revision. Gate E establishes them
for the artifact that will be published. CI `gate-e-evidence` downloads the
verified candidate, reconstructs the release revision with `git archive`, and
grafts the unpacked candidate crate over `crates/lspf`. Every journey it then
runs compiles the packaged bytes rather than the workspace sources:
compatibility against the frozen public interface, admission overload, handler
and outgoing request timeouts, peer disconnect, stdio child cleanup, the
reference server's own test suite, and the editor journey driven through an
installed `lspf-markdown` selected by `LSPF_MARKDOWN_SERVER`.

Gate E also reads
[`ci/release-blockers-v1.json`](./ci/release-blockers-v1.json), the register of
blockers that validation discovered. Each entry names a severity, an owner, a
tracked issue, and a disposition. A framework-owned P0 or P1 that is still
`open` has no disposition and fails the gate; one that is `accepted` passes but
is reported as a maintainer judgment rather than absorbed into a passing
result. Any framework gap the editor journeys tracked must appear in the
register, so it cannot stay empty while validation is finding problems.

Reproduce the evidence for a downloaded candidate with:

```bash
gh run download RUN_ID \
  --name lspf-1.0.0-release-candidate \
  --dir target/downloaded-candidate
revision="$(jq -r .revision target/downloaded-candidate/candidate-metadata.json)"
bash ci/run-gate-e-evidence.sh \
  "$revision" \
  "https://github.com/meymchen/lspf/actions/runs/RUN_ID" \
  target/downloaded-candidate \
  target/reproduced-gate-e-evidence
```

## Publication and the release record

Publishing is the only irreversible step in the pipeline, so CI
`release-publish` is the only job behind a protected environment: a maintainer
approves it after reading the candidate report and the Gate E evidence.

`cargo publish` packages the checked-out source rather than uploading an
artifact, so the crate it sends is the validated candidate only if packaging
the release revision still reproduces those bytes. The job proves that first,
with `ci/check-repackaged-candidate.sh`, and stops before minting a token if
the comparison fails: afterwards a mismatch would be a permanently published
crate that no evidence covers. Only then does it mint a short-lived crates.io
token through trusted publishing, publish the validated revision, download the
crate the registry serves, and check that crate against the candidate hash a
second time.

What survives the release is the release record: one archive holding the
candidate bundle, the crate the registry served, provenance, the SBOM and its
attestation, the documentation archive, both changelogs, the policies that were
in force at the release revision, and Gate A through E evidence. Both hash
lists are self-verifying, so a later reader can check the archive without
trusting the job that produced it:

```bash
gh run download RUN_ID \
  --name lspf-1.0.0-release-record \
  --dir target/downloaded-release-record
revision="$(jq -r .revision target/downloaded-release-record/release-record.json)"
bash ci/check-release-record.sh "$revision" target/downloaded-release-record
```

The release record proves that the published crate is the validated candidate.
It does not prove that publishing was the right decision; the maintainer
authorization that released it is recorded as a human judgment.

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability. Use GitHub's
[private vulnerability reporting form](https://github.com/meymchen/lspf/security/advisories/new).
Include the affected versions and features, impact, reproduction steps or a
proof of concept, and any mitigation you have found. Reports should avoid real
user data and credentials.

The maintainers will:

1. acknowledge the report within three business days;
2. provide an initial severity and affected-version assessment within seven
   calendar days;
3. send an update at least every fourteen calendar days while the report
   remains open;
4. coordinate the fix and disclosure date with the reporter, then publish a
   GitHub security advisory that names affected versions, mitigations, and the
   fixed release.

Resolution time depends on severity and the work needed for a safe fix, so
there is no fixed resolution deadline. Credit is given with the reporter's
consent. If a report is not a vulnerability, the maintainers will close the
private report with an explanation and may suggest a public issue for the
underlying bug.

GitHub private vulnerability reporting is enabled for this repository. CI and
the release workflow run the supply-chain security gate, which rejects known
dependency advisories, unapproved licenses, mutable Action references, and
workflow permissions outside the repository policy. No automation replaces
private reports about vulnerabilities in lspf's own code.
