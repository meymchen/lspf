# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/meymchen/lspf/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/meymchen/lspf/releases/tag/v0.1.2
[0.1.1]: https://github.com/meymchen/lspf/releases/tag/v0.1.1
[0.1.0-alpha.3]: https://github.com/meymchen/lspf/releases/tag/v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/meymchen/lspf/releases/tag/v0.1.0-alpha.2
