# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

`0.3` completes the typed registration surface the 0.2 `Server` started. A
server registers standard features through the sealed descriptor catalog in
[`lspf::features`](https://docs.rs/lspf/latest/lspf/features/): each
descriptor fixes the wire method, the typed parameters and result, and the
capability contribution at once, and `ServerCapabilities` are generated from
the same registrations that dispatch — no handwritten capability object and
no framework modification. The
[features, capabilities, and the workspace][guide-features] guide and the
[`lspf-hello`][hello] template server walk the complete journey, and
[`crates/lspf-hello/tests/e2e.rs`][hello-e2e] verifies it over a real stdio
connection.

### Added

- The sealed feature catalog under `lspf::features`, covering the stable LSP
  3.17 features: navigation (declaration, definition, type definition,
  implementation, references, document highlight, document symbol, workspace
  symbol, call and type hierarchies, moniker, linked editing range),
  presentation (signature help, formatting, on-type formatting, document
  color, color presentation, folding range, selection range, inline value,
  inlay hints), semantic tokens (full, delta, and range), diagnostics
  (document and workspace), file operations (will/did create, rename, and
  delete), and watched files. Each request feature registers through
  `ServerBuilder::feature`, each notification feature through
  `ServerBuilder::feature_notification`.
- Typed Commands beneath `workspace/executeCommand`, registered through
  `ServerBuilder::command`. Each registered name merges into one
  de-duplicated `executeCommandProvider` whose `commands` list preserves
  registration order (ADR 0022).
- The multi-root `Workspace` handle: client info, client capabilities,
  initialization options, root URI, and workspace folders — verbatim and
  order-preserving — plus the latest raw configuration settings and the
  trace level, reachable through `Context::workspace()`.
- `Workspace::text_document`, resolving a document snapshot from editor-open
  text first and then the connection's configured `FileProvider`;
  `MemoryFileProvider` for virtual resources and tests, and `OsFileProvider`
  (with a byte-limit builder) for `file:` URIs on native targets.
- The lifecycle hooks `on_initialize`, `on_initialized`, and `on_exit`.
- `ServerBuilder::configure_initialize`, the single initialization-dependent
  registration transaction: the callback sees read-only `InitializeParams`
  and a transactional `InitializeRegistrar`, and either the whole
  transaction commits or initialization fails.
- `ServerBuilder::text_document_sync`, accepting protocol-owned
  `TextDocumentSyncOptions`, with incremental synchronization as the
  default.
- Typed post-validation hooks for `textDocument/willSave` and
  `textDocument/didSave`, plus `features::will_save_wait_until()`.

### Changed

- Capability derivation is now strict. Families that share one singular
  capability field (diagnostics, semantic tokens, file operations, each
  resolve/prepare family) merge their contributions under that field;
  contributions that disagree, or a dependent feature registered without its
  base (a dangling `resolveProvider` or `prepareProvider`), fail the build
  with `BuildError::ConflictingCapability` instead of resolving by
  registration order.
- Command registration and an explicit `workspace/executeCommand` request
  handler are a `BuildError::ExecuteCommandConflict` — the method routes
  either to the command table or to the handler, never both.
- Advertised document-sync capabilities, accepted notifications, document
  mutations, and hooks now share one effective configuration. Save-related
  fields are inferred from registrations, while conflicting explicit
  `false` values fail the build.
- The connection's `Workspace` now owns the document store; handlers reach
  it through the read-only `DocumentsView` from `ctx.documents()`, and the
  workspace's later mutations (folder and configuration changes, trace
  updates) land before user hooks observe them.

[guide-features]: https://github.com/meymchen/lspf/blob/main/docs/guides/features-and-workspace.md
[hello]: https://github.com/meymchen/lspf/blob/main/crates/lspf-hello/src/main.rs
[hello-e2e]: https://github.com/meymchen/lspf/blob/main/crates/lspf-hello/tests/e2e.rs

## [0.2.0] - 2026-08-02

`0.2.0` replaces the `LanguageServer` trait with the built `Server`, the typed
`Router`, the `Service`/`Layer` stack, and the connection-owned protocol engine.
It is a **breaking release with no adapter and no deprecation cycle**: the 0.1
surface is removed rather than phased out, and no feature flag or runtime
setting restores it. The
[0.1-to-0.2 migration guide][migration-0.1-to-0.2] maps every 0.1 construct
onto its 0.2 replacement, and each of its `0.2` examples is compiled as a
doc-test against this release.

[migration-0.1-to-0.2]: https://github.com/meymchen/lspf/blob/main/docs/migrations/0.1-to-0.2.md

### Removed

- The `LanguageServer` trait, together with its lifecycle and document methods
  (`initialize`, `initialized`, `shutdown`, `exit`, `text_document_did_*`), its
  `documents()` getter, its `server_capabilities()` override, and its
  `TEXT_DOCUMENT_SYNC` capability constant. Registrations move to
  `Server::builder`; capabilities are generated from those registrations.
- The trait-based dispatcher behind that surface. `ProtocolEngine` — reached
  through `Server::serve` and `lspf::stdio(server).serve()` — is the only
  dispatch path, and it owns the routes and the protocol state outright.
- `lspf::serve` and `lspf::serve_with_limit`. A built `Server` carries its own
  concurrency policy and reports an `Outcome`, so `Server::serve(transport)`
  replaces both. `DEFAULT_CONCURRENCY_LIMIT` remains as the `ServerBuilder`
  default.
- The public `Documents` handle and every escape hatch around it:
  `Documents::new`, `open`, `close`, `save`, `apply_incremental_change`,
  `get`, `set_position_encoding`, and the position-conversion methods. The
  connection's protocol engine owns the store; handlers read it through the
  read-only `DocumentsView` from `ctx.documents()`. `Document` and
  `PositionEncoding` remain public.
- `Context::for_test_notification`, the hidden constructor that let user code
  build a `Context` outside a connection.

### Added

- `ClientError::Cancelled` and `ClientError::IdExhausted` variants.
- `DocumentsView`, the read-only document handle a handler reaches through
  `Context::documents()`: retained document lookup, position conversion under
  the negotiated encoding, and no mutation operation.
- Post-mutation document hooks. Registering `textDocument/didOpen`,
  `didChange`, or `didClose` with `.notification::<N>(handler)` records the
  connection's one hook for that built-in instead of a Router route: the
  protocol engine decodes and mutates first, then the hook observes the result.
  A second hook for the same method is a `BuildError::DuplicateMethod`, and a
  decode or built-in validation failure skips the hook without ending the
  connection. `textDocument/didSave` has no framework mutation in 0.2 and stays
  an ordinary typed notification route.
- `ServerCapabilities` now advertise `textDocumentSync` (incremental) as a
  protocol-owned field, alongside the negotiated position encoding. Document
  sync is an engine built-in, so a server that registers nothing still tells
  the editor to send `didOpen`, `didChange`, and `didClose`.

### Changed

- `lspf::stdio(...)` now takes a built `Server` rather than a `LanguageServer`,
  and `serve()` resolves to `lspf::Result<Outcome>` instead of terminating the
  process — a binary maps `Outcome::code()` onto its own process disposition.
  Concurrency policy moves with it: the cap is set by
  `ServerBuilder::concurrency_limit`, and the stdio builder no longer carries
  the knob.
- `Context::documents()` now returns `&DocumentsView` rather than `&Documents`,
  so handlers can read the connection's documents but never mutate them.
- A `didChange` notification's content changes now apply all-or-nothing: a
  rejected change leaves the document at the revision the last accepted
  notification produced, rather than at a half-applied one.
- Outbound request IDs are now allocated from a monotonic, never-reused
  sequence instead of wrapping around, so a late response for an abandoned
  request can never complete a later request.
- Abandoning an enqueued outbound request now emits one typed
  `$/cancelRequest` notification for the request's ID; requests dropped before
  enqueue emit nothing.
- Session close now completes every pending outbound request with
  `ClientError::Cancelled` instead of a generic session-closed error.

## [0.1.2] - 2026-06-22

### Fixed

- Include `README.md` in the `lspf` crate package by adding the `readme`
  field to `crates/lspf/Cargo.toml` so it renders on crates.io.

## [0.1.1] - 2026-06-22

### Changed

- Bump release version from `0.1.0-alpha.3` to `0.1.1` and publish `lspf` to
  crates.io.

## [0.1.0-alpha.3] - 2026-06-17

### Added

- `cargo coverage` alias in `.cargo/config.toml` for local HTML coverage reports.
- CI coverage job that generates and uploads HTML/LCOV reports as artifacts.
- Documentation: test coverage glossary entry in `CONTEXT.md` and coverage
  instructions in `README.md`.

## [0.1.0-alpha.2] - 2026-06-12

### Added

- Walking skeleton: `stdio` transport and lifecycle dispatcher (`src/` core
  with `server` trait, `dispatcher`, `context`, `stdio` transport, `error`,
  `raw`).
- Outgoing helpers: per-request `Context` send channel and
  `publish_diagnostics`.
- `examples/hello` runnable example and `tests/smoke.rs` integration test.
- Domain documentation: `CONTEXT.md` glossary and 14 architecture decision
  records under `docs/adr/` (ADRs 0001–0014, including ADR 0014 covering
  protocol types sourced from the `lsp-types` crate).
- Project documentation and agent configuration: `README.md`, `CLAUDE.md`,
  and the `tools/` directory.
- Toolchain pinning and lint configuration: `rust-toolchain.toml`,
  `rustfmt.toml`, `clippy.toml`.

[Unreleased]: https://github.com/meymchen/lspf/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/meymchen/lspf/releases/tag/v0.2.0
[0.1.2]: https://github.com/meymchen/lspf/releases/tag/v0.1.2
[0.1.1]: https://github.com/meymchen/lspf/releases/tag/v0.1.1
[0.1.0-alpha.3]: https://github.com/meymchen/lspf/releases/tag/v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/meymchen/lspf/releases/tag/v0.1.0-alpha.2
