# Operating an lspf endpoint

One `Server` or `Client` owns one LSP connection. Its `ResourcePolicy`, queues,
deadlines, and tracing identity end with that connection. A Server's
`Documents`, `Notebooks`, and `Workspace` are connection-scoped too. A process
that serves several peers constructs one endpoint per peer and owns any shared
cache or index outside lspf.

This guide covers production budgets, observability, deployment, shutdown, and
troubleshooting. The authoritative support matrix and maintenance window are
in [`SECURITY.md`](../../SECURITY.md).

## Start with the finite defaults

`ResourcePolicy::default()` applies these per-connection limits:

| Field | Default | What remains charged |
| --- | ---: | --- |
| `max_inbound_requests` | 64 | Admitted requests until their single completion path wins. |
| `max_outbound_messages` | 1,024 | Accepted messages until the Transport send finishes or fails. |
| `max_outbound_bytes` | 16 MiB | Encoded JSON-RPC envelope bodies in the outbound queue. |
| `max_documents` | 1,024 | Open text documents, including notebook cells. |
| `max_document_bytes` | 64 MiB | Retained text across those documents. |
| `max_notebooks` | 256 | Notebook-level metadata, including empty notebooks. |
| `outbound_request_timeout` | 30 seconds | Each typed request sent to the peer. |
| `handler_timeout` | 30 seconds | Each admitted inbound request handler. |

All numeric limits and enabled deadlines must be greater than zero. A bad
policy is a `BuildError`, before any I/O starts. Only
`outbound_request_timeout` may be `None`; disable it only when another owner
provides a firm deadline.

Install the whole policy rather than scattering unrelated knobs:

```rust
use std::time::Duration;

use lspf::{ResourcePolicy, Server};

# struct State;
# fn main() {
let policy = ResourcePolicy {
    max_inbound_requests: 32,
    max_outbound_messages: 256,
    max_outbound_bytes: 4 * 1024 * 1024,
    max_documents: 2_000,
    max_document_bytes: 128 * 1024 * 1024,
    max_notebooks: 128,
    outbound_request_timeout: Some(Duration::from_secs(10)),
    handler_timeout: Duration::from_secs(20),
};

let server = Server::builder(State)
    .resource_policy(policy)
    .build()
    .expect("the production resource policy is valid");
# let _ = server;
# }
```

`ServerBuilder::concurrency_limit` remains as shorthand for
`max_inbound_requests`; do not set both in different configuration paths.

## Tune from retained cost and latency

Measure realistic document sizes, concurrent editor requests, response sizes,
and slow-peer behavior before raising a limit. The defaults are safety bounds,
not throughput targets.

- If inbound capacity rejects bursts, first inspect handler latency and
  cancellation. Raising the limit makes more work and memory live at once.
- If optional notifications hit `ClientError::OutboundOverloaded`, coalesce or
  drop obsolete application updates before enlarging the queue.
- If a required response cannot fit, the connection closes as
  `Outcome::WriterFailed`; the endpoint never silently drops required traffic.
- If document admission fails, the built-in leaves the previous snapshot
  untouched and skips the post-mutation hook. Notebook cells use the same
  document budgets.
- If deadlines expire during valid work, separate queue delay, external I/O,
  blocking CPU work, and a genuinely slow peer before choosing a new value.

Use [`docs/performance-baselines.md`](../performance-baselines.md) for the
repository's measured request workloads and
[`docs/soak-journeys.md`](../soak-journeys.md) for bounded-memory stress
journeys. They are reference workloads, not production sizing promises.

## Keep blocking work bounded

All handlers are async. Move blocking libraries to `spawn_blocking`, check the
request `CancellationToken` between work units, and put an application-owned
limit around expensive jobs. The inbound request budget prevents unbounded
protocol admission; it does not limit the runtime's blocking pool, subprocesses,
database connections, or memory held in application state.

The [errors and cancellation guide](./errors-and-cancellation.md) has a
cancellation-aware blocking example.

## Emit useful, payload-free telemetry

At `lspf=trace`, the framework emits stable `rpc message`,
`resource budget changed`, `deadline changed`, `request completed`, and
`connection closed` events. The exact fields and enum values are listed in the
[README tracing schema](../../README.md#repository-development).

Route stdio server logs to stderr. Stdout is the LSP byte stream; one human
line on stdout corrupts framing. For a process supervisor, prefer structured
stderr and include your own build or instance identity outside protocol
payloads.

Register `ServerBuilder::on_error` for metrics that must not parse logs. The
hook provides a `ConnectionFailureCategory` plus redacted connection and call
identity. It deliberately omits parameters, results, document contents, wire
bytes, panic payloads, and underlying error text.

The default tracing events have the same payload rule. Application events can
still leak source text, paths, request arguments, or remote messages, so review
those fields before sending logs to a shared backend.

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
are listed exhaustively in [`SECURITY.md`](../../SECURITY.md). Other targets
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
  [`docs/public-interface.md`](../public-interface.md), not implied by an ADR
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

For API availability, use the [frozen public interface](../public-interface.md)
and warning-free [docs.rs reference](https://docs.rs/lspf/latest/lspf/). For a
reproducible protocol failure, start with the in-memory journeys in the
[testing guide](./testing.md).
