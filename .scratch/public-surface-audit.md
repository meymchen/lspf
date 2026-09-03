# Public surface audit before the 1.0 freeze

This audit records the crate-root surface after the pre-freeze cleanups through
issues #240, #241, #245, #248, #252, and #253. “Keep” means freeze the current
name and role for 1.0. No rename or delete verdicts remain to file as follow-up
work.

## Public modules and constant

| Export | Verdict | Reason |
| --- | --- | --- |
| `features` | Keep | The sealed feature descriptors are the primary typed registration catalog. |
| `fuzzing` | Keep | This feature-gated module is the supported fuzz-harness integration surface. |
| `testing` | Keep | This feature-gated module supplies the public in-memory test harness. |
| `types` | Keep | Protocol types and marker compatibility traits need one discoverable namespace. |
| `DEFAULT_CONCURRENCY_LIMIT` | Keep | Users need the documented default when reasoning about connection admission. |

The generated protocol names nested below `types` are owned by
`gen-lsp-types`; this audit covers the crate-root export boundary rather than
re-auditing that generated LSP 3.18 model.

## Server construction and handlers

| Export | Verdict | Reason |
| --- | --- | --- |
| `SharedHandler` | Keep | Public builder bounds expose this target-aware callable contract, so it is now documented rather than hidden. |
| `InitializeRegistrar` | Keep | Initialize-time conditional registration needs a deliberately narrower builder view. |
| `Server` | Keep | This is the built server and central inbound endpoint. |
| `ServerBuilder` | Keep | This is the primary server configuration interface. |
| `ServerContext` | Keep | Handlers need one connection-scoped route to framework state. |
| `CancellationToken` | Keep | Request cancellation is part of every public request-handler signature. |

## Outbound client endpoint

| Export | Verdict | Reason |
| --- | --- | --- |
| `ClientHandle` | Keep | Server handlers need typed outbound notifications and requests. |
| `TelemetryEventParams` | Keep | Telemetry accepts arbitrary serializable values through a stable explicit wrapper. |
| `Client` | Keep | This is the configured outbound LSP endpoint. |
| `ClientBuilder` | Keep | Reverse handlers and client policy need a construction interface. |
| `ClientConnection` | Keep | Callers must own and drive a generic initialized client connection. |
| `ClientContext` | Keep | Reverse handlers need protocol-scoped context without editor-owned state. |
| `ServerHandle` | Keep | Callers need typed calls and lifecycle control for the remote server. |

## Documents, notebooks, workspace, and files

| Export | Verdict | Reason |
| --- | --- | --- |
| `Document` | Keep | Handlers need immutable snapshots of synchronized text documents. |
| `DocumentsView` | Keep | The read-only document lookup boundary prevents user mutation of protocol state. |
| `PositionEncoding` | Keep | Position conversion must expose the negotiated encoding. |
| `Notebook` | Keep | Handlers need immutable snapshots of synchronized notebook structure. |
| `NotebooksView` | Keep | The read-only notebook lookup boundary mirrors document ownership. |
| `Workspace` | Keep | Handlers need current initialization, folder, configuration, and file state. |
| `WorkspaceError` | Keep | File-backed workspace lookup failures require typed handling. |
| `FileProvider` | Keep | Applications need a host-independent unopened-file seam. |
| `MemoryFileProvider` | Keep | Virtual resources and deterministic tests need the in-memory implementation. |
| `EmptyFileProvider` | Keep | WASM callers need an explicit no-filesystem default provider. |
| `OsFileProvider` | Keep | Native callers need the standard filesystem implementation. |
| `OsFileProviderBuilder` | Keep | Native callers need to configure filesystem read limits. |

## Results, errors, and connection failures

| Export | Verdict | Reason |
| --- | --- | --- |
| `Outcome` | Keep | Serving returns protocol exit status without terminating the process. |
| `Result` | Keep | The crate-wide result alias keeps public serving signatures concise. |
| `Error` | Keep | Top-level server and transport operations need one error type. |
| `BuildError` | Keep | Static registration and configuration failures need typed inspection. |
| `ClientError` | Keep | Outbound send and request failures need typed inspection. |
| `LspError` | Keep | Handlers need to construct protocol response errors. |
| `ProgressError` | Keep | Work-done progress lifecycle misuse needs typed inspection. |
| `ConnectionDirection` | Keep | Failure hooks need to distinguish inbound from outbound work. |
| `ConnectionFailure` | Keep | The error hook needs one stable, redacted failure value. |
| `ConnectionFailureCategory` | Keep | Observers need actionable failure classification without sensitive details. |
| `ConnectionFailureContext` | Keep | Observers need safe connection and call identity. |
| `ConnectionRequestId` | Keep | Failure context must preserve numeric IDs while redacting string contents. |

## Features, partial results, and progress

| Export | Verdict | Reason |
| --- | --- | --- |
| `FeatureSpec` | Keep | Server registration needs the sealed request-feature contract. |
| `NotificationFeatureSpec` | Keep | Server registration needs the sealed notification-feature contract. |
| `PartialResultRequest` | Keep | Custom requests need an opt-in association with partial-result payloads. |
| `PartialResultSink` | Keep | Handlers need a typed, budget-aware partial-result sender. |
| `ProgressHandle` | Keep | Callers need to report and finish an active work-done operation. |
| `ProgressOptions` | Keep | Callers need to configure progress begin metadata. |

## Raw protocol and resource policy

| Export | Verdict | Reason |
| --- | --- | --- |
| `JsonRpcError` | Keep | Custom transports and tests need the wire-level error object. |
| `RawMessage` | Keep | The public transport contract needs a framed protocol message type. |
| `RequestId` | Keep | Layers, raw messages, and diagnostics share request identity. |
| `ResourcePolicy` | Keep | Callers need to configure per-connection resource limits. |
| `ResourcePolicyField` | Keep | Build errors must identify an invalid policy field. |

## Runtime and service layers

| Export | Verdict | Reason |
| --- | --- | --- |
| `TaskFuture` | Keep | Public boxed futures need a name for target-dependent future mobility, so it is now documented rather than hidden. |
| `TaskSend` | Keep | Public handler, provider, and transport bounds need one target-dependent mobility marker, so it is now documented rather than hidden. |
| `CallKind` | Keep | Layers need to distinguish requests from notifications. |
| `IncomingCall` | Keep | Layers need a stable method-erased inbound call value. |
| `Layer` | Keep | User middleware composition is a supported extension seam. |
| `Next` | Keep | A layer needs an explicit handle for invoking the inner service. |
| `ServiceFuture` | Keep | Layer implementations need the target-aware erased future type. |
| `ServiceResult` | Keep | Layers need one method-erased success or LSP error result. |

## Transports

| Export | Verdict | Reason |
| --- | --- | --- |
| `Transport` | Keep | Custom adapters need the public message-framed transport contract. |
| `TransportError` | Keep | Reader and writer halves need a shared typed failure vocabulary. |
| `TransportReader` | Keep | Custom transports must expose asynchronous message reads. |
| `TransportWriter` | Keep | Custom transports must expose asynchronous message writes and close. |
| `stdio` | Keep | Native servers need the concise standard-input/output constructor. |
| `StdioBuilder` | Keep | Stdio resource limits need an explicit configuration interface. |
| `StdioReader` | Keep | The concrete reader is part of the public split transport type. |
| `StdioTransport` | Keep | Users need a nameable configured stdio transport. |
| `StdioWriter` | Keep | The concrete writer is part of the public split transport type. |
| `ChildConnection` | Keep | A supervised child process and its protocol connection need joint ownership. |
| `ChildError` | Keep | Child launch, protocol, and supervision failures need typed inspection. |
| `ChildOutput` | Keep | Callers need the supervised process output after completion. |
| `tcp` | Keep | Native servers need the concise TCP-listener constructor. |
| `TcpBuilder` | Keep | TCP limits and listener policy need an explicit configuration interface. |
| `TcpReader` | Keep | The concrete reader is part of the public split transport type. |
| `TcpTransport` | Keep | Users need a nameable TCP transport. |
| `TcpWriter` | Keep | The concrete writer is part of the public split transport type. |
| `websocket` | Keep | Native servers need the concise WebSocket constructor. |
| `WebSocketBuilder` | Keep | WebSocket limits and handshake policy need explicit configuration. |
| `WebSocketReader` | Keep | The concrete reader is part of the public split transport type. |
| `WebSocketTransport` | Keep | Users need a nameable WebSocket transport. |
| `WebSocketWriter` | Keep | The concrete writer is part of the public split transport type. |
| `worker_channel` | Keep | Worker-hosted WASM needs the concise message-channel constructor. |
| `WorkerChannelBuilder` | Keep | Worker-channel limits need an explicit configuration interface. |
| `WorkerChannelReader` | Keep | The concrete reader is part of the public split transport type. |
| `WorkerChannelTransport` | Keep | Users need a nameable Worker channel transport. |
| `WorkerChannelWriter` | Keep | The concrete writer is part of the public split transport type. |

## Remaining LSP 3.17 references

Every remaining `3.17` mention in Rust and Markdown was checked against the
vendored LSP 3.18 metaModel or identified as decision history:

- `ClientHandle` method annotations remain 3.17 for diagnostics, messages,
  telemetry, progress, show-document, configuration, workspace folders,
  capability registration, diagnostic refresh, inlay-hint refresh, and
  inline-value refresh because their metaModel `since` value is 3.17 or
  earlier. The folding-range and text-document-content refresh annotations
  remain 3.18.
- Document and notebook synchronization, initialized parameters, progress,
  and watched-files comments describe protocol facts that remained unchanged
  in 3.18.
- ADRs 0014 and 0032 record the historical protocol-type dependency state;
  ADRs 0024 and 0034 retain the prior 3.17 catalog decision only to explain
  how the current 3.18 boundary superseded it.
