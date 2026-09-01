# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- [**breaking**] Remove the historical outbound-warning-threshold constant and
  its dedicated `BuildError` variant. Configure the outbound message budget
  through `ResourcePolicy::max_outbound_messages`; the retained builder
  shorthand now reports zero through `InvalidResourcePolicy`.

## [0.10.0](https://github.com/meymchen/lspf/compare/v0.9.2...v0.10.0) - 2026-09-01

### Fixed

- *(fuzzing)* assert well-formed JSON rather than Value representability ([#265](https://github.com/meymchen/lspf/pull/265))

### Other

- Migrate protocol type base to metaModel-generated types ([#275](https://github.com/meymchen/lspf/pull/275))
- Record generated protocol types decision and fix Runtime boundary ([#274](https://github.com/meymchen/lspf/pull/274))
- fix SonarCloud analysis warnings

## [0.9.2](https://github.com/meymchen/lspf/compare/v0.9.1...v0.9.2) - 2026-08-31

### Added

- Add `MemoryTransport::pair_uncaptured` for long-lived and high-volume tests
  that must not retain a clone of every message as wire history.
- Run bounded-memory request, cancellation, edit, progress, slow-peer,
  reconnect, and shutdown soak journeys with retained time-series evidence
  ([#186](https://github.com/meymchen/lspf/pull/186)) ([#232](https://github.com/meymchen/lspf/pull/232)).
- Publish a Client adoption guide with compiling custom-Transport and
  supervised-stdio-child walkthroughs
  ([#225](https://github.com/meymchen/lspf/pull/225)).
- Launch, drive, and deterministically reclaim stdio language-server children
  through `ClientBuilder::spawn` and `ChildConnection`
  ([#178](https://github.com/meymchen/lspf/pull/178)) ([#223](https://github.com/meymchen/lspf/pull/223)).
- Build a first-party Markdown link language server ([#234](https://github.com/meymchen/lspf/pull/234))
- Publish reusable protocol testing utilities ([#227](https://github.com/meymchen/lspf/pull/227))
- Expose ServerHandle and ClientContext reverse interactions ([#221](https://github.com/meymchen/lspf/pull/221))

### Fixed

- Reject unpaired UTF-16 surrogate escapes in forwarded `params` and `result`
  with a -32700 parse error instead of passing them through to the peer. Such
  an escape denotes a character with no UTF-8 encoding, so forwarding it could
  hand a peer JSON that peer cannot decode
  ([#262](https://github.com/meymchen/lspf/pull/262)).

### Other

- Establish reproducible performance baselines ([#231](https://github.com/meymchen/lspf/pull/231))
- Model protocol session concurrency ([#230](https://github.com/meymchen/lspf/pull/230))
- Fuzz protocol and document boundaries ([#228](https://github.com/meymchen/lspf/pull/228))
- Record Gate C endpoint evidence ([#226](https://github.com/meymchen/lspf/pull/226))
- Prove public-only Server and Client conformance ([#224](https://github.com/meymchen/lspf/pull/224))

## [0.9.1](https://github.com/meymchen/lspf/compare/v0.9.0...v0.9.1) - 2026-08-26

### Other

- Complete typed Client exchange over custom Transport ([#218](https://github.com/meymchen/lspf/pull/218))
- Extract the shared private protocol session ([#217](https://github.com/meymchen/lspf/pull/217))

### Changed

- [**breaking**] Rename the handler context to `ServerContext` and the
  server-to-client peer handle to `ClientHandle`, without compatibility aliases.

## [0.8.1](https://github.com/meymchen/lspf/compare/v0.8.0...v0.8.1) - 2026-08-26

### Other

- Record Gate B bounded-resource evidence ([#214](https://github.com/meymchen/lspf/pull/214))
- Test deterministic overload and close races ([#213](https://github.com/meymchen/lspf/pull/213))
- Add connection error hooks and pin Rust 1.98 ([#212](https://github.com/meymchen/lspf/pull/212))
- Expose stable connection tracing schema ([#211](https://github.com/meymchen/lspf/pull/211))
- Apply handler deadlines through the completion gate ([#210](https://github.com/meymchen/lspf/pull/210))

### Changed

- [**breaking**] Raise the MSRV from Rust 1.96 to 1.98 and pin development and
  CI to the exact Rust 1.98.0 toolchain.

### Added

- [**breaking**] Bound outbound queues by message count and encoded bytes,
  returning `ClientError::OutboundOverloaded` when ordinary sends exceed a
  connection budget.
- [**breaking**] Apply the configured outbound-request deadline and return
  `ClientError::Timeout` after cancelling an expired request. Set
  `ResourcePolicy::outbound_request_timeout` to `None` to disable the deadline.
- Enforce the finite inbound handler deadline through the request completion
  gate. Layers can override a request's timeout through `IncomingCall`, and
  expiry cooperatively cancels the handler before returning `ServerCancelled`.

## [0.6.0](https://github.com/meymchen/lspf/compare/v0.5.3...v0.6.0) - 2026-08-24

### Other

- Adopt one connection ResourcePolicy ([#203](https://github.com/meymchen/lspf/pull/203))

## [0.5.3](https://github.com/meymchen/lspf/compare/v0.5.2...v0.5.3) - 2026-08-24

### Other

- Enforce warning-free public API documentation ([#156](https://github.com/meymchen/lspf/pull/156))
- publish support and security contract ([#154](https://github.com/meymchen/lspf/pull/154)) ([#194](https://github.com/meymchen/lspf/pull/194))
- refresh and bilingualize user documentation

## [0.5.2](https://github.com/meymchen/lspf/compare/v0.5.1...v0.5.2) - 2026-08-23

### Other

- Fix CI: markdownlint MD041 for skill docs and flaky cancel race test
- Improve editor LSP debugging workflow ([#152](https://github.com/meymchen/lspf/pull/152))
- Add runnable LSP feature examples ([#150](https://github.com/meymchen/lspf/pull/150))

## [0.5.1](https://github.com/meymchen/lspf/compare/v0.5.0...v0.5.1) - 2026-08-20

### Other

- Add on_shutdown lifecycle hook ([#149](https://github.com/meymchen/lspf/pull/149))
- add regressions from mature LSP implementations ([#147](https://github.com/meymchen/lspf/pull/147))

### Added

- Add `ServerBuilder::on_shutdown`, an async lifecycle hook that gates the
  protocol-owned shutdown transition and leaves the connection running when it
  returns `LspError`.

## [0.5.0](https://github.com/meymchen/lspf/compare/v0.4.0...v0.5.0) - 2026-08-19

### Other

- Ship Transport selection guides and buildable examples ([#134](https://github.com/meymchen/lspf/pull/134))
- Enforce the target and Cargo-feature matrix in CI ([#133](https://github.com/meymchen/lspf/pull/133))
- Run the shared Transport conformance journey in a Worker ([#132](https://github.com/meymchen/lspf/pull/132))
- Serve one WASM worker channel through MessagePort ([#131](https://github.com/meymchen/lspf/pull/131)) ([#143](https://github.com/meymchen/lspf/pull/143))
- Serve one WebSocket connection as framed JSON ([#130](https://github.com/meymchen/lspf/pull/130)) ([#142](https://github.com/meymchen/lspf/pull/142))
- Serve one TCP connection through Content-Length framing ([#129](https://github.com/meymchen/lspf/pull/129)) ([#141](https://github.com/meymchen/lspf/pull/141))
- Make stdio the Transport conformance reference ([#128](https://github.com/meymchen/lspf/pull/128)) ([#140](https://github.com/meymchen/lspf/pull/140))
- Run the portable protocol kernel on TokioRuntime and WasmRuntime ([#127](https://github.com/meymchen/lspf/pull/127)) ([#139](https://github.com/meymchen/lspf/pull/139))
- Fence optional adapters behind the 0.5 Cargo feature graph ([#126](https://github.com/meymchen/lspf/pull/126)) ([#135](https://github.com/meymchen/lspf/pull/135))

## [0.4.0](https://github.com/meymchen/lspf/compare/v0.3.0...v0.4.0) - 2026-08-15

### Added

- add the client workspace query helpers ([#105](https://github.com/meymchen/lspf/pull/105)) ([#118](https://github.com/meymchen/lspf/pull/118))
- add the window interaction request helpers ([#104](https://github.com/meymchen/lspf/pull/104)) ([#117](https://github.com/meymchen/lspf/pull/117))
- [**breaking**] add the complete outgoing notification helper surface ([#102](https://github.com/meymchen/lspf/pull/102)) ([#116](https://github.com/meymchen/lspf/pull/116))

### Other

- Ship the 0.4 outgoing-client guide and stdio verification ([#112](https://github.com/meymchen/lspf/pull/112))
- Observe outbound queue depth without dropping messages ([#111](https://github.com/meymchen/lspf/pull/111))
- Handle client cancellation of work-done progress ([#110](https://github.com/meymchen/lspf/pull/110))
- Deliver a leak-free work-done progress lifecycle ([#109](https://github.com/meymchen/lspf/pull/109))
- Gate proposed workspace refresh helpers behind a Cargo feature ([#108](https://github.com/meymchen/lspf/pull/108))
- Update ([#107](https://github.com/meymchen/lspf/pull/107))
- No data supplied ([#106](https://github.com/meymchen/lspf/pull/106))
- enforce MSRV 1.96 and reduce coverage artifact retention to 3 days
- widen inbound completion watchdogs for instrumented CI runs ([#114](https://github.com/meymchen/lspf/pull/114))
- [**breaking**] carry the full JsonRpcError in ClientError::Remote ([#101](https://github.com/meymchen/lspf/pull/101)) ([#113](https://github.com/meymchen/lspf/pull/113))
- promote 0.3.0 changelog and drop the unused 0.1.x records ([#99](https://github.com/meymchen/lspf/pull/99))

## [0.3.0](https://github.com/meymchen/lspf/compare/v0.2.1...v0.3.0) - 2026-08-14

### Other

- Ship the 0.3 examples, guides, and stdio verification ([#82](https://github.com/meymchen/lspf/pull/82)) ([#98](https://github.com/meymchen/lspf/pull/98))
- Complete lifecycle hooks and enforce the stable catalog boundary ([#81](https://github.com/meymchen/lspf/pull/81)) ([#97](https://github.com/meymchen/lspf/pull/97))
- Read file URIs through OsFileProvider ([#80](https://github.com/meymchen/lspf/pull/80)) ([#96](https://github.com/meymchen/lspf/pull/96))
- Add navigation and lookup feature descriptors ([#79](https://github.com/meymchen/lspf/pull/79)) ([#95](https://github.com/meymchen/lspf/pull/95))
- Resolve workspace documents through file providers ([#78](https://github.com/meymchen/lspf/pull/78)) ([#94](https://github.com/meymchen/lspf/pull/94))
- Add editing and presentation feature descriptors ([#93](https://github.com/meymchen/lspf/pull/93))
- Add hierarchy and semantic token feature families ([#76](https://github.com/meymchen/lspf/pull/76)) ([#92](https://github.com/meymchen/lspf/pull/92))
- Configure document synchronization end to end ([#91](https://github.com/meymchen/lspf/pull/91))
- Add resolve and prepare text-document feature families ([#74](https://github.com/meymchen/lspf/pull/74)) ([#90](https://github.com/meymchen/lspf/pull/90))
- Serve workspace requests and file-operation routes ([#73](https://github.com/meymchen/lspf/pull/73)) ([#89](https://github.com/meymchen/lspf/pull/89))
- Merge document and workspace diagnostics ([#87](https://github.com/meymchen/lspf/pull/87))

## [0.2.1](https://github.com/meymchen/lspf/compare/v0.2.0...v0.2.1) - 2026-08-10

### Other

- Apply workspace mutations before user hooks ([#86](https://github.com/meymchen/lspf/pull/86))
- Complete the typed Command wire contract ([#70](https://github.com/meymchen/lspf/pull/70)) ([#85](https://github.com/meymchen/lspf/pull/85))
- Introduce family-aware capability merging through completion resolve ([#69](https://github.com/meymchen/lspf/pull/69)) ([#84](https://github.com/meymchen/lspf/pull/84))
- Establish the complete Workspace snapshot and normalized URI identity ([#83](https://github.com/meymchen/lspf/pull/83))
- release v0.2.0 ([#53](https://github.com/meymchen/lspf/pull/53))

## [0.2.0](https://github.com/meymchen/lspf/releases/tag/v0.2.0) - 2026-08-02

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
- [WIP] Implement concurrent typed ClientHandle requests handling ([#61](https://github.com/meymchen/lspf/pull/61))
- Send typed ClientHandle notifications from handlers ([#60](https://github.com/meymchen/lspf/pull/60))
- Run user dispatch through the fixed Service stack ([#59](https://github.com/meymchen/lspf/pull/59))
- Guarantee exactly-once inbound request completion ([#58](https://github.com/meymchen/lspf/pull/58))
