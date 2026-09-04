---
title: Deploy and troubleshoot endpoints
description: Choose a process topology, own shutdown, and diagnose endpoint failures.
---

Deployment starts from the same rule as resource management: one `Server` or `Client` owns one connection. This guide applies that boundary to process topology and shutdown.

## Choose a deployment shape

| Deployment | Endpoint and Transport | Operational owner |
| --- | --- | --- |
| Editor launches one native server | `Server` with default `stdio` | Editor or plugin restarts the process; the server keeps stdout protocol-only. |
| Service accepts native TCP peers | One `Server` and `TcpTransport::from_stream` per accepted socket | Application owns the outer accept loop, authentication, TLS, and shared state. |
| Service accepts WebSocket peers | One `Server` and `WebSocketTransport::from_stream` per established stream | Application owns HTTP upgrade policy, authentication, TLS, and reconnect. |
| Browser or Node Worker | `Server` with `worker-channel` | JavaScript host creates and transfers the `MessagePort`, then owns Worker termination. |
| Application launches a language server | `ClientBuilder::spawn` | `ChildConnection` owns protocol driving, stderr drain, termination escalation, and reap. |
| Existing channel or runtime host | Custom `Transport` with `runtime-tokio` or `wasm` | Adapter owns framing, size limits, authentication, and channel lifecycle. |

The first-party TCP and WebSocket builders accept one connection and then drop
their listener. They do not provide a multi-tenant server loop. First-party
Transports also do not add TLS, authentication, reconnect, or load balancing.
Wrap and authenticate the stream before it becomes a custom Transport when
those policies are required.

Supported native hosts, WASM, Rust versions, and Cargo feature combinations
are listed exhaustively in [`SECURITY.md`](https://github.com/meymchen/lspf/blob/main/SECURITY.md). Other targets
may compile, but they are not part of the support promise.

## Shut down with one owner

For a server, drive `serve` until the peer sends `exit` or the Transport ends.
The returned `Outcome` records whether shutdown was orderly. lspf never calls
`std::process::exit` itself.

For a Client over a custom Transport, one task drives
`ClientConnection::serve` while a single lifecycle owner calls
`ServerHandle::shutdown` followed by `exit`. Use `disconnect` for local
teardown when graceful protocol traffic is unwanted.

For a stdio child, prefer `ChildConnection::shutdown` or `wait`. Both return
the protocol outcome, OS status, and bounded stderr. Drop still starts cleanup,
but it cannot return that evidence to the application.

Connection closure rejects new work, resolves pending requests, cancels owned
handler tasks, drains already-admitted outbound messages, and joins connection
tasks. Do not retain connection-scoped handles in global state after the owner
finishes.

## Known limitations

- lspf is an LSP protocol framework, not an editor UI, project model, parser,
  index, cache, or language implementation.
- `Client` dispatches typed reverse traffic but is not a complete editor or
  extension-host framework. The application owns UI, workspace, filesystem,
  diagnostics presentation, and restart policy.
- Native execution uses Tokio; custom native executors are unsupported. WASM
  execution targets browser or Node Workers on `wasm32-unknown-unknown`.
- There is no built-in metrics exporter. Use tracing and `on_error` with an
  application-owned metrics backend.
- Transport helpers serve one connection. Multi-client acceptance and shared
  cross-connection state belong to the host application.
- Deferred protocol capabilities and non-frozen exports are inventoried in
  [the frozen public interface](https://github.com/meymchen/lspf/blob/main/docs/public-interface.md), not implied by an ADR
  or by a generated protocol type being present.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| A stdio server hangs when run in a terminal | It is waiting for `Content-Length`-framed LSP input. Use an editor, the Client tutorial, or a scripted peer. |
| The editor reports malformed headers or JSON | Remove every stdout log and banner; send application logs to stderr. |
| `serve` returns `RuntimeRequired` | Start it inside a Tokio runtime, such as `#[tokio::main]`. lspf does not start a native runtime implicitly. |
| A feature never receives requests | Confirm its descriptor was registered and inspect the generated initialize capabilities. Custom raw routes do not advertise standard capabilities. |
| A document hook does not run | Confirm document sync is enabled and the notification passed validation and resource admission. Notebook cell changes invoke notebook hooks, not text-document hooks. |
| Requests return `ServerCancelled` under load | Inspect the message: capacity exhaustion means inbound admission is full; `handler deadline expired` means the request exceeded its deadline. |
| Sends return `OutboundOverloaded` | The optional message exceeded the message-count or encoded-byte budget. Coalesce stale output or tune from measured queue pressure. |
| Diagnostics or positions are shifted around emoji | Use `ctx.documents().position_encoding()` and the view's conversion helpers; UTF-16 counts surrogate pairs as two units. |
| A Client request times out | Ensure the connection driver is running, the peer implements the method, and the outbound deadline fits observed latency. |
| A supervised child exits early | Inspect `ChildOutput::outcome`, OS status, stderr, and `stderr_truncated`; pending requests resolve as cancelled. |
| TCP or WebSocket serves only one peer | This is the first-party builder contract. Put endpoint construction inside an application-owned accept loop. |
| A WASM build selects stdio, TCP, or WebSocket | Disable default features and select `wasm` for a custom Transport or `worker-channel` for `MessagePort`. |

For API availability, use the [frozen public interface](https://github.com/meymchen/lspf/blob/main/docs/public-interface.md)
and warning-free [docs.rs reference](https://docs.rs/lspf/latest/lspf/). For a
reproducible protocol failure, start with the in-memory journeys in the
[testing guide](../testing/).
