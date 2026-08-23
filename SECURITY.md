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

The minimum supported Rust version (MSRV) is **1.96.0**. The supported compiler
range is Rust 1.96.0 through the latest stable Rust release. Nightly and beta
toolchains are useful for early warning, but are not supported toolchains.

The workspace `rust-version` field is the source of truth. On Rust 1.96.0, CI
`feature-contract` compiles every documented feature selection for the Linux
host and `wasm32-unknown-unknown`. CI `msrv` checks the release-oriented native
matrix and the whole workspace with default features on the same compiler.
The other Rust jobs use stable, including CI `native-matrix`, `test`, and
`wasm`.

An MSRV increase is a breaking change. Before 1.0 it may happen only in a new
minor release; after 1.0 it requires a new major release. The release notes
must name the old and new MSRV. A patch release must not raise the MSRV.

## Supported hosts and targets

The following table is exhaustive. "Supported" means maintainers accept bug
reports and intend to fix regressions on the maintained release line.

| Host | Rust target | Status | Enforcement gate |
| --- | --- | --- | --- |
| Linux | `x86_64-unknown-linux-gnu` | Supported | CI `native-matrix` and `test`; the cross-OS lifecycle gate is tracked by #155 |
| Windows | `x86_64-pc-windows-msvc` | Supported | Cross-OS lifecycle gate tracked by #155 |
| macOS | `x86_64-apple-darwin` | Supported | Cross-OS lifecycle gate tracked by #155 |
| macOS | `aarch64-apple-darwin` | Supported | Cross-OS lifecycle gate tracked by #155 |
| Browser or Node Worker | `wasm32-unknown-unknown` | Supported | CI `wasm` and `feature-contract` |

Until #155 closes, Windows and macOS support is a maintainer commitment rather
than a continuously tested claim. Other operating systems, architectures,
WASI targets, and embedded or `no_std` environments are unsupported. Reports
with a small, portable fix are welcome, but accepting such a fix does not add
the host or target to this matrix.

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

The current automated release jobs are Release-plz workflow jobs
`release-plz-pr` and `release-plz-release`; they prepare release metadata and
publish releases but do not prove API compatibility. The machine-readable
compatibility gate is tracked by #157. Until that gate lands, reviewers
enforce this section from the public diff and changelog.

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

The planned public API compatibility gate in #157 will detect removal of
deprecated and non-deprecated API. CI `markdownlint` checks the policy and
other Markdown today; the warning-free public documentation gate is tracked
by #156.

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

GitHub private vulnerability reporting is enabled for this repository. The
automated dependency advisory and license gate is tracked by #160; until it
lands, dependency review is manual. No automation replaces private reports
about vulnerabilities in lspf's own code.
