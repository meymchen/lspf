# Support, compatibility, and security policy

[English](./SECURITY.md) | [Simplified Chinese](./SECURITY.zh-CN.md)

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
selection. Features in the same supported native row may be combined;
`proposed` may be added to any supported row. No other cross-target
combination is supported.

| Target family | Feature selection | Status | Enforcement gate |
| --- | --- | --- | --- |
| Native | default features or `stdio` | Supported stdio Transport | CI `msrv`, `native-matrix`, and `test` |
| Native | `tcp` | Supported TCP Transport | CI `msrv`, `native-matrix`, and `test` |
| Native | `websocket` | Supported WebSocket Transport | CI `msrv`, `native-matrix`, and `test` |
| Native | any combination of `stdio`, `tcp`, and `websocket` | Supported | CI `msrv` row `all-native-features` and CI `test` |
| Native | `runtime-tokio` without a first-party adapter | Supported for a custom Transport | CI `feature-contract` |
| Native | no runtime or Transport feature | Supported for protocol-only compilation, not serving | CI `feature-contract` |
| Native | `worker-channel` | Intentionally invalid | CI `feature-contract` checks the compile-time diagnostic |
| `wasm32-unknown-unknown` | `wasm` without a first-party adapter | Supported for a custom Transport | CI `wasm` |
| `wasm32-unknown-unknown` | `worker-channel` | Supported MessagePort Transport; implies `wasm` | CI `wasm` and `feature-contract` |
| `wasm32-unknown-unknown` | no `wasm` | Intentionally invalid | CI `feature-contract` checks the compile-time diagnostic |
| `wasm32-unknown-unknown` | default features or `stdio` | Unsupported | Not applicable; this selection is outside the support contract |
| `wasm32-unknown-unknown` | `tcp` or `websocket` | Intentionally invalid | CI `feature-contract` checks the compile-time diagnostics |
| Any supported row | add `proposed` | Supported as an unstable API surface; it never selects a runtime or Transport | CI `msrv` row `proposed`, CI `native-matrix`, and CI `test` |

The [Transport guide](./docs/guides/transports.md) gives build commands and
describes the APIs enabled by each feature. CI `feature-contract` compiles
every supported selection with and without `proposed`. It also checks that
Transport-specific dependencies do not leak into the default build and that
`proposed` stays independent of every Transport.

Pull requests use a fast feedback path: `feature-contract`, maximal workspace
tests, public documentation, packaging, compatibility, security, WASM, and the
cross-platform lifecycle run before merge. The release-oriented `msrv` and
native test matrices, coverage, and Gate A/B evidence run after a push to
`main`. A newer commit on the same pull request cancels its older CI run.

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

APIs behind `proposed` track draft LSP specifications. A patch release does
not intentionally break them, but a minor release may change or remove them
without a deprecation cycle. Such changes still appear in the changelog.

CI `public API compatibility` compares the crate with the latest version on
crates.io, which is the maintained baseline. The exhaustive `feature-contract`
and public-docs gates own combination correctness. Compatibility checks the
core and additive maximal API surfaces for native and WASM targets: native
without features, native with `stdio,tcp,websocket,proposed`, WASM with
`wasm,proposed`, and WASM with `worker-channel,proposed`. The job uploads
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
