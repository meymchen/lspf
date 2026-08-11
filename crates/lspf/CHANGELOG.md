# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1](https://github.com/meymchen/lspf/compare/v0.2.0...v0.2.1) - 2026-08-10

### Other

- Apply workspace mutations before user hooks ([#86](https://github.com/meymchen/lspf/pull/86))
- Complete the typed Command wire contract ([#70](https://github.com/meymchen/lspf/pull/70)) ([#85](https://github.com/meymchen/lspf/pull/85))
- Introduce family-aware capability merging through completion resolve ([#69](https://github.com/meymchen/lspf/pull/69)) ([#84](https://github.com/meymchen/lspf/pull/84))
- Establish the complete Workspace snapshot and normalized URI identity ([#83](https://github.com/meymchen/lspf/pull/83))
- release v0.2.0 ([#53](https://github.com/meymchen/lspf/pull/53))

## [0.2.0](https://github.com/meymchen/lspf/compare/v0.1.3...v0.2.0) - 2026-08-02

### Added

- commit initialize-time registrations transactionally ([#42](https://github.com/meymchen/lspf/pull/42)) ([#57](https://github.com/meymchen/lspf/pull/57))
- register notifications, commands, hover, and completion (#40, #41) ([#56](https://github.com/meymchen/lspf/pull/56))
- build and serve a typed custom request ([#39](https://github.com/meymchen/lspf/pull/39)) ([#55](https://github.com/meymchen/lspf/pull/55))
- route task execution through runtime ([#54](https://github.com/meymchen/lspf/pull/54))

### Fixed

- *(client)* close OutboundRegistry to new inserts atomically with drain
- *(dispatcher)* complete pending client requests with Cancelled on session close
- resolve clippy 1.96 lints (collapsible_if, derivable_impls)
- recover from malformed JSON-RPC envelopes ([#52](https://github.com/meymchen/lspf/pull/52))

### Other

- Contract the legacy API and finalize 0.2 ([#51](https://github.com/meymchen/lspf/pull/51)) ([#66](https://github.com/meymchen/lspf/pull/66))
- Migrate stdio, lspf-hello, and user documentation ([#50](https://github.com/meymchen/lspf/pull/50)) ([#65](https://github.com/meymchen/lspf/pull/65))
- Expose DocumentsView through post-mutation hooks ([#49](https://github.com/meymchen/lspf/pull/49)) ([#64](https://github.com/meymchen/lspf/pull/64))
- Converge shutdown, exit, EOF, and writer failure ([#48](https://github.com/meymchen/lspf/pull/48)) ([#63](https://github.com/meymchen/lspf/pull/63))
- Cancel outgoing requests without registry leaks
- [WIP] Implement concurrent typed Client requests handling ([#61](https://github.com/meymchen/lspf/pull/61))
- Send typed Client notifications from handlers ([#60](https://github.com/meymchen/lspf/pull/60))
- Run user dispatch through the fixed Service stack ([#59](https://github.com/meymchen/lspf/pull/59))
- Guarantee exactly-once inbound request completion ([#58](https://github.com/meymchen/lspf/pull/58))

## [0.1.3](https://github.com/meymchen/lspf/compare/v0.1.2...v0.1.3) - 2026-07-22

### Other

- automate releases with release-plz ([#28](https://github.com/meymchen/lspf/pull/28))
