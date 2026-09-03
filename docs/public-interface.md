# The frozen 1.0 public interface

This document is the inventory of everything `lspf` promises to downstream
code at 1.0. It names each public export once, records the target and feature
selection that makes it available, and states what the export is for. Anything
the crate exposes but this file does not freeze is listed under
[Not part of the frozen interface](#not-part-of-the-frozen-interface).

Each named item includes its complete documented public associated surface:
constructors and methods, trait members, public fields, enum variants, generic
bounds, and function signatures. Those details remain readable in the generated
API documentation rather than being duplicated here; `cargo-semver-checks`
compares them with the released baseline. The tables below decide which items
belong to the intended interface, while that structural comparison freezes the
exact Rust API of every selected item.

The inventory is enforced, not descriptive. `ci/check-public-interface.sh`
compares it against `crates/lspf/src/lib.rs`, `crates/lspf/src/testing.rs`,
`crates/lspf/src/features.rs`, `crates/lspf/tests/catalog.rs`,
`crates/lspf/Cargo.toml`, and [`SECURITY.md`](../SECURITY.md), and fails when
the code and this file disagree in either direction. See
[How the freeze is enforced](#how-the-freeze-is-enforced).

For what a change to a frozen item costs — the semantic-versioning rules, the
deprecation window, and the approval registry for a reviewed break — see
[`SECURITY.md`](../SECURITY.md).

## Availability

Every frozen item is available under one of these keys. The key is the Cargo
`cfg` that gates the export in `crates/lspf/src/lib.rs`; the checker rejects
an export whose gate is not listed here.

| Availability | Cargo cfg | Meaning |
| --- | --- | --- |
| `any` | *(ungated)* | Every supported target and feature selection, including a protocol-only build with no runtime. |
| `runtime` | `#[cfg(any(feature = "runtime-tokio", target_arch = "wasm32"))]` | Wherever a `Runtime` exists to serve a connection: native with `runtime-tokio`, or Worker-hosted WASM. |
| `native-runtime` | `#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]` | Native targets with the Tokio runtime, where a real filesystem exists. |
| `wasm32` | `#[cfg(target_arch = "wasm32")]` | Worker-hosted WASM only, where no filesystem exists. |
| `stdio` | `#[cfg(all(feature = "stdio", not(target_arch = "wasm32")))]` | The native `stdio` Transport and its supervised child process. |
| `tcp` | `#[cfg(all(feature = "tcp", not(target_arch = "wasm32")))]` | The native single-client TCP Transport. |
| `websocket` | `#[cfg(all(feature = "websocket", not(target_arch = "wasm32")))]` | The native single-client WebSocket Transport. |
| `worker-channel` | `#[cfg(all(feature = "worker-channel", target_arch = "wasm32"))]` | The Worker-hosted WASM `MessagePort` Transport. |
| `testing` | `#[cfg(all(feature = "testing", not(target_arch = "wasm32")))]` | The native downstream test harness. |
| `fuzzing` | `#[cfg(all(feature = "fuzzing", not(target_arch = "wasm32")))]` | The repository's own fuzz harness. Nothing under this key is frozen. |

## Crate-root modules and constants

| Item | Availability | Role |
| --- | --- | --- |
| `features` | any | The sealed typed registration catalog; the primary way a server declares what it serves. |
| `testing` | testing | The in-memory Transport, wire capture, virtual clock, and protocol journeys for downstream tests. |
| `types` | any | The one discoverable namespace for LSP protocol types and the marker compatibility traits. |
| `DEFAULT_CONCURRENCY_LIMIT` | any | The documented in-flight cap a `Server` applies when it sets none of its own. |

## Server construction and dispatch

| Item | Availability | Role |
| --- | --- | --- |
| `Server` | any | The built server: one LSP connection and its initialization lifecycle. |
| `ServerBuilder` | any | The static registration surface for handlers, features, commands, layers, and connection policy. |
| `InitializeRegistrar` | any | The deliberately narrower builder view inside the single `configure_initialize` transaction. |
| `ServerContext` | any | The cheap-to-clone handle every handler receives; the only route from a handler to framework state. |
| `SharedHandler` | any | The target-aware callable contract that public builder bounds name; documented because users depend on its shape. |
| `CancellationToken` | any | The cancellation signal in every request-handler signature; re-exported from `tokio-util`. |

## Outbound client endpoint

| Item | Availability | Role |
| --- | --- | --- |
| `ClientHandle` | any | The typed handle for server-to-client notifications and correlated requests, reached through `ctx.client()`. |
| `TelemetryEventParams` | any | The explicit wrapper that lets `telemetry/event` carry an arbitrary serializable value. |
| `Client` | runtime | The configured outbound LSP endpoint over a caller-provided Transport or supervised stdio child. |
| `ClientBuilder` | runtime | The construction surface for reverse handlers, initialization inputs, and client policy. |
| `ClientConnection` | runtime | The initialized, exclusively owned client endpoint and its inbound protocol driver. |
| `ClientContext` | runtime | The protocol-only context passed to reverse handlers; editor state stays caller-owned. |
| `ServerHandle` | runtime | The cloneable handle for typed client-to-server calls and Client lifecycle transitions. |

## Documents, notebooks, workspace, and files

| Item | Availability | Role |
| --- | --- | --- |
| `Document` | any | An immutable snapshot of one synchronized text document. |
| `DocumentsView` | any | The read-only document lookup that gives user code no mutation operation. |
| `PositionEncoding` | any | The negotiated position encoding a handler needs for offset conversion. |
| `Notebook` | any | An immutable snapshot of one synchronized notebook's structure. |
| `NotebooksView` | any | The read-only notebook lookup that mirrors document ownership. |
| `Workspace` | any | The cloneable handle to initialization metadata, roots, folders, configuration, and trace level. |
| `WorkspaceError` | any | The typed failure of a file-backed workspace lookup. |
| `FileProvider` | any | The host-independent seam for resolving resources that are not open in the editor. |
| `MemoryFileProvider` | any | The in-memory provider for virtual resources and deterministic tests. |
| `EmptyFileProvider` | wasm32 | The explicit no-filesystem default for Worker-hosted WASM. |
| `OsFileProvider` | native-runtime | The standard filesystem provider for native hosts. |
| `OsFileProviderBuilder` | native-runtime | The configuration surface for the filesystem provider's read limits. |

## Results, errors, and connection outcome

| Item | Availability | Role |
| --- | --- | --- |
| `Outcome` | runtime | How one connection ended, plus its LSP exit code; serving reports it instead of exiting the process. |
| `Result` | any | The crate-wide result alias that keeps public serving signatures concise. |
| `Error` | any | The one error type for top-level server and transport operations. |
| `BuildError` | any | Typed inspection of static registration and configuration failures. |
| `ClientError` | any | Typed inspection of outbound send, deadline, overload, and correlation failures. |
| `LspError` | any | The protocol response error a handler constructs. |
| `ProgressError` | any | Typed inspection of work-done progress lifecycle misuse. |
| `ConnectionFailure` | any | The stable, redacted failure value the connection error hook observes. |
| `ConnectionFailureCategory` | any | Actionable failure classification without sensitive detail. |
| `ConnectionFailureContext` | any | Safe connection and call identity for an observed failure. |
| `ConnectionDirection` | any | Distinguishes inbound from outbound work in a failure report. |
| `ConnectionRequestId` | any | Preserves numeric request IDs while redacting peer-controlled string contents. |

## Features, partial results, and progress

| Item | Availability | Role |
| --- | --- | --- |
| `FeatureSpec` | any | The sealed request-feature contract every catalog descriptor satisfies. |
| `NotificationFeatureSpec` | any | The sealed notification-feature contract every catalog descriptor satisfies. |
| `PartialResultRequest` | any | The opt-in association between a custom request and its partial-result payload. |
| `PartialResultSink` | any | The request-scoped, typed, budget-aware destination for result chunks. |
| `ProgressHandle` | any | The handle that reports and ends one active work-done operation. |
| `ProgressOptions` | any | The begin-time metadata for a work-done progress lifecycle. |

## Raw protocol and resource policy

| Item | Availability | Role |
| --- | --- | --- |
| `RawMessage` | any | The framed JSON-RPC envelope the public Transport contract carries. |
| `JsonRpcError` | any | The wire-level error object custom transports and tests need. |
| `RequestId` | any | The request identity shared by raw messages, layers, and diagnostics. |
| `ResourcePolicy` | any | The single connection-owned declaration of finite budgets and deadlines. |
| `ResourcePolicyField` | any | Identifies which policy field a build error rejected. |

## Layers and task mobility

| Item | Availability | Role |
| --- | --- | --- |
| `Layer` | any | The user middleware composition seam that wraps user dispatch only. |
| `Next` | any | The explicit handle a layer uses to invoke the inner service. |
| `IncomingCall` | any | The method-erased inbound call value a layer sees. |
| `ServiceResult` | any | The method-erased success or LSP error a layer returns. |
| `ServiceFuture` | any | The target-aware erased future a layer implementation returns. |
| `CallKind` | any | Distinguishes a request from a notification inside a layer. |
| `TaskFuture` | any | The name for target-dependent boxed-future mobility that public signatures require. |
| `TaskSend` | any | The sealed target-dependent mobility marker in public handler, provider, and transport bounds. |

## Transports

| Item | Availability | Role |
| --- | --- | --- |
| `Transport` | any | The public message-framed transport contract a custom adapter implements. |
| `TransportReader` | any | The asynchronous message-read half a custom transport exposes. |
| `TransportWriter` | any | The asynchronous message-write and close half a custom transport exposes. |
| `TransportError` | any | The shared typed failure vocabulary of both halves. |
| `stdio` | stdio | The concise standard-input/output constructor for a native server. |
| `StdioTransport` | stdio | The nameable configured stdio transport. |
| `StdioBuilder` | stdio | The explicit configuration surface for stdio resource limits. |
| `StdioReader` | stdio | The concrete reader half of the split stdio transport. |
| `StdioWriter` | stdio | The concrete writer half of the split stdio transport. |
| `ChildConnection` | stdio | Joint ownership of one supervised child process and its initialized client connection. |
| `ChildError` | stdio | Typed inspection of child launch, protocol, and supervision failures. |
| `ChildOutput` | stdio | The supervised process's exit status, outcome, and captured stderr after completion. |
| `tcp` | tcp | The concise TCP-listener constructor for a native server. |
| `TcpTransport` | tcp | The nameable TCP transport. |
| `TcpBuilder` | tcp | The explicit configuration surface for TCP limits and listener policy. |
| `TcpReader` | tcp | The concrete reader half of the split TCP transport. |
| `TcpWriter` | tcp | The concrete writer half of the split TCP transport. |
| `websocket` | websocket | The concise WebSocket constructor for a native server. |
| `WebSocketTransport` | websocket | The nameable WebSocket transport. |
| `WebSocketBuilder` | websocket | The explicit configuration surface for WebSocket limits and handshake policy. |
| `WebSocketReader` | websocket | The concrete reader half of the split WebSocket transport. |
| `WebSocketWriter` | websocket | The concrete writer half of the split WebSocket transport. |
| `worker_channel` | worker-channel | The concise `MessagePort` constructor for a Worker-hosted WASM server. |
| `WorkerChannelTransport` | worker-channel | The nameable Worker channel transport. |
| `WorkerChannelBuilder` | worker-channel | The explicit configuration surface for Worker channel limits. |
| `WorkerChannelReader` | worker-channel | The concrete reader half of the split Worker channel transport. |
| `WorkerChannelWriter` | worker-channel | The concrete writer half of the split Worker channel transport. |

## `lspf::testing`

The `testing` feature is native-only and opt-in. Every item below is frozen at
the same level as the crate root; the [testing guide](./guides/testing.md)
shows them in use.

| Item | Role |
| --- | --- |
| `MemoryTransport` | The in-memory `Transport` a downstream test hands to a real `Server` or `Client`. |
| `MemoryReader` | The concrete reader half of the in-memory transport. |
| `MemoryWriter` | The concrete writer half of the in-memory transport. |
| `ScriptedPeer` | The other end of the in-memory transport, driven message by message from the test. |
| `WireCapture` | The ordered record of every message that crossed the transport seam. |
| `WireEvent` | One captured message with its sequence number and direction. |
| `WireDirection` | Whether a captured message was sent to or received from the peer. |
| `ServerJourney` | The standard initialize, initialized, shutdown, and exit exchange against a real `Server`. |
| `ClientJourney` | The same standard exchange against a real `Client`, exposing its `ServerHandle`. |
| `JourneyError` | The typed failure of arranging or driving either journey. |
| `VirtualClock` | Deterministic control of the same Tokio clock that connection deadlines use. |

## `lspf::types`

`lspf::types` re-exports the generated LSP 3.18 model from `gen-lsp-types`
wholesale (ADR 0032). The generated names are owned by that crate and change
with the metaModel rather than with this inventory; `tests/catalog.rs` pins
them against the vendored `fixtures/lsp_meta_model_3_18_0.json`.

Three things in that namespace are lspf's own and are frozen here.

The interface checker permits only the generated model re-export, the two
marker submodules and traits described below, and the inventoried aliases. Any
other public declaration or re-export in this namespace fails the freeze gate.

`types::request::Request` and `types::notification::Notification` are lspf's
marker traits. A custom request or notification implements them directly, and
a blanket implementation adapts every generated marker, so a handler
registration accepts both. The request and notification submodules also
re-export the generated markers under LSP's specification names — the names
the guides and examples use — which the catalog fixture pins method by method.

The type aliases below give a self-describing name to a generated type whose
bare name is ambiguous inside one flat namespace, or whose name states a
structure rather than the protocol position it occupies. They are the names
the README, the guides, the examples, and the tests use. They are frozen as
part of the interface rather than treated as migration leftovers.

| Alias | Generated type | Why the alias is frozen |
| --- | --- | --- |
| `ApplyWorkspaceEditResponse` | `ApplyWorkspaceEditResult` | Names the response half of `workspace/applyEdit` in handler signatures. |
| `CodeActionOrCommand` | `CodeActionResponse` | States the union a `textDocument/codeAction` handler actually returns. |
| `ColorProviderOptions` | `DocumentColorOptions` | Matches the `colorProvider` capability field a registration configures. |
| `DiagnosticServerCapabilities` | `DiagnosticProvider` | Matches the `diagnosticProvider` capability rather than the provider structure. |
| `DocumentDiagnosticReportResult` | `DocumentDiagnosticReport` | Distinguishes the request result from the report variants nested inside it. |
| `GotoDefinitionParams` | `DefinitionParams` | Keeps the goto family's parameter names parallel across definition, declaration, type definition, and implementation. |
| `GotoDefinitionResponse` | `DefinitionResponse` | Keeps the goto family's result names parallel for the same reason. |
| `HoverContents` | `Contents` | `Contents` alone says nothing in a flat namespace shared by every protocol type. |
| `InlayHintLabel` | `Label` | `Label` alone is ambiguous; inlay hints are the only position that uses it. |
| `InlayHintTooltip` | `Tooltip` | `Tooltip` alone is ambiguous for the same reason. |
| `PrepareRenameResponse` | `PrepareRenameResult` | Names the response half of `textDocument/prepareRename` in handler signatures. |
| `ReferencesOptions` | `ReferenceOptions` | Matches the plural `textDocument/references` method the options configure. |
| `SemanticTokenModifier` | `SemanticTokenModifiers` | The singular reads correctly at each use site; the generated name is the enum of all modifiers. |
| `SemanticTokenType` | `SemanticTokenTypes` | The singular reads correctly at each use site for the same reason. |
| `SemanticTokensResult` | `SemanticTokens` | Names the full-request result position, so a handler signature says which request it answers. |
| `SemanticTokensRangeResult` | `SemanticTokens` | Names the range-request result position, which carries the same type as the full request. |
| `SemanticTokensFullDeltaResult` | `SemanticTokensDeltaResponse` | Names the full-delta result position alongside the other two. |
| `TextDocumentSyncCapability` | `TextDocumentSync` | Matches the `textDocumentSync` capability field a server sets. |
| `TextDocumentSyncSaveOptions` | `Save` | `Save` alone is ambiguous; these are the save options inside text-document sync. |
| `WorkspaceDiagnosticReportResult` | `WorkspaceDiagnosticReport` | Distinguishes the request result from the report variants nested inside it. |
| `WorkspaceServerCapabilities` | `WorkspaceOptions` | Matches the `workspace` capability field a server sets. |

## `lspf::features`

The catalog is the complete stable LSP 3.18 inbound surface (ADR 0024,
ADR 0034). Rather than repeat every descriptor and its paired opaque descriptor
type here, the freeze holds it to three properties that are failing tests:

- `crates/lspf/tests/catalog.rs` registers every descriptor in one server and
  pins the resulting `ServerCapabilities` JSON byte-for-byte against
  `crates/lspf/tests/fixtures/full_catalog_capabilities.json`, then measures
  the advertised fields against the vendored metaModel. A descriptor that is
  added, renamed, or loses its capability contribution fails that test.
- `ci/check-public-interface.sh` checks that every `pub fn` in
  `crates/lspf/src/features.rs` is registered by that journey, so a new
  descriptor cannot enter the catalog without entering the pinned fixture.
- The same checker requires every public descriptor type to be the return type
  of one of those registered functions and rejects any other public type in the
  module, so an implementation type cannot become public accidentally.

`FeatureSpec` and `NotificationFeatureSpec` are sealed. Downstream code selects
descriptors; it does not add them.

## Public dependencies

These crates appear in frozen signatures, so their types are part of the
compatibility contract and a major bump in any of them is a breaking change
for `lspf`.

| Crate | Where it appears | Availability |
| --- | --- | --- |
| `gen-lsp-types` | The whole `lspf::types` namespace and every typed handler signature. | any |
| `serde` | Bounds on custom request and notification parameters and results. | any |
| `serde_json` | `JsonRpcError::data`. | any |
| `bytes` | The raw JSON payloads inside `RawMessage`. | any |
| `tokio-util` | `CancellationToken`. | any |
| `tokio` | `TcpTransport::from_stream` and the listener address bound. | tcp |

`ropey` backs `Document` but never appears in a public signature (ADR 0005).
`futures-channel`, `futures-util`, `tracing`, `thiserror`, and the WASM
bindings are internal.

## Not part of the frozen interface

The crate exposes these; 1.0 does not promise them.

| Item | Availability | Why it is not frozen |
| --- | --- | --- |
| `fuzzing` | fuzzing | The repository's own cargo-fuzz harness surface. It is hidden from documentation, the `fuzzing` feature is outside the support contract in `SECURITY.md`, and its shape follows the fuzz targets. |

## Deferred capabilities

These are the things a reader might reasonably expect and will not find at 1.0.
Each is a deliberate boundary with the decision that set it, not an oversight.

- **Proposed and draft LSP methods.** The catalog stops at the complete stable
  LSP 3.18 surface. No proposed method appears in the catalog or in the default
  capabilities (ADR 0024), and the Cargo feature that once gated proposed work
  no longer exists. A server that needs one registers it as a custom request or
  notification, which contributes nothing to `ServerCapabilities`.
- **Adding a catalog descriptor from outside the crate.** `FeatureSpec` and
  `NotificationFeatureSpec` are sealed.
- **Implementing the dispatch `Service`.** `Layer` and `Next` are the public
  extension seam; the `Service` trait the framework adapts registrations into
  stays crate-private (ADR 0019). A layer sees decoded calls, never transport
  bytes, and cannot intercept lifecycle, cancellation, or document mutation.
- **Choosing or naming a `Runtime`.** Exactly two implementations exist and the
  compile target picks between them; there is no runtime-selection API and the
  trait is not nameable (ADR 0020). `TaskFuture` and `TaskSend` express the
  resulting target-dependent bounds and are the only part of that machinery
  users can name.
- **Synchronous handlers, and `tower` interop.** The framework is `async fn`
  end to end with no sync escape hatch (ADR 0001), and `Layer` is lspf's own
  narrower trait, deliberately not interoperable with `tower::Layer`
  (ADR 0010).
- **More than one connection per `Server`.** A `Server` owns exactly one LSP
  connection; a second connection requires a second `Server`, and connection
  state is never shared (ADR 0017).
- **Mutating framework-owned protocol state.** `DocumentsView` and
  `NotebooksView` have no mutation operation, and user code neither constructs
  nor stores `Documents`, `ServerContext`, or `ClientContext`. Registering a
  built-in document or workspace notification records a post-mutation hook
  instead of replacing the mutation.
- **Position encodings beyond UTF-8 and UTF-16.** `PositionEncoding` has two
  variants and negotiation chooses between them (ADR 0016).
- **`testing` and `worker-channel` outside their targets.** `testing` is
  native-only and `worker-channel` is `wasm32`-only; each unsupported
  combination fails with its own compile-time diagnostic rather than a
  confusing dependency error.
- **Targets and feature selections outside the support contract.** WASI,
  `no_std`, and every host and target absent from the matrix in
  [`SECURITY.md`](../SECURITY.md) are unsupported, as is any feature selection
  that document does not list.

## How the freeze is enforced

| What | Gate |
| --- | --- |
| This inventory matches the code, in both directions | `ci/check-public-interface.sh`, self-tested by `ci/test-check-public-interface.sh` |
| Downstream crates can name the frozen root, native, WASM-only, testing, and owned-alias exports | `crates/lspf/tests/frozen_interface.rs` and `crates/lspf/tests/frozen_wasm_interface.rs`; the latter compiles for the real `wasm32-unknown-unknown` target |
| Every feature descriptor is callable from a downstream crate and contributes the intended capability | `crates/lspf/tests/catalog.rs` |
| Full downstream Server and Client journeys still work through public API alone | `crates/lspf/tests/public_conformance.rs` |
| A frozen item's methods, members, fields, variants, bounds, or signatures are not removed or changed incompatibly | CI `public API compatibility` (`ci/check-public-api.sh`), with reviewed breaks recorded in `ci/public-api-breaking-approvals.json` |
| Every frozen item has warning-free documentation on every supported surface | CI `public docs` (`ci/check-public-docs.sh`) |
| Every documented feature selection compiles, and unsupported ones fail as designed | CI `feature contract` (`ci/check-feature-contract.sh`) |
| The catalog still matches the LSP 3.18 metaModel | `crates/lspf/tests/catalog.rs` |
