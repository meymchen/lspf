# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/meymchen/lspf/compare/v0.1.0-alpha.3...HEAD
[0.1.0-alpha.3]: https://github.com/meymchen/lspf/releases/tag/v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/meymchen/lspf/releases/tag/v0.1.0-alpha.2
