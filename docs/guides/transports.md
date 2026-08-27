# Choosing and implementing a Transport

[English](./transports.md) | [简体中文](./transports.zh-CN.md)

A `Server` owns one LSP connection. A Transport is the message-framed channel
that carries that connection's JSON-RPC envelopes. Choose the Transport from
the host that owns the connection; handler registration and business logic do
not change with that choice.

## Selection guide

| Host and connection | Choose | Cargo invocation | Wire framing |
| --- | --- | --- | --- |
| Editor launches a native process | stdio | default features, or `--no-default-features --features stdio` | `Content-Length` |
| One native client connects to a port | TCP | `--no-default-features --features tcp` | `Content-Length` |
| One native WebSocket client connects | WebSocket | `--no-default-features --features websocket` | One JSON envelope per text or binary message |
| Browser or Node host transfers a port to a WASM Worker | worker-channel | `--target wasm32-unknown-unknown --no-default-features --features worker-channel` | One JSON envelope per `MessagePort` message |
| An embedding already has another message channel | custom Transport | Enable `runtime-tokio` on native or `wasm` on WASM, plus only dependencies your adapter needs | Defined by the adapter |

The first-party TCP and WebSocket builders bind once, accept one client, and
then drop their listener. To serve another connection, construct another
`Server`; connection state is intentionally not shared.

## Cargo features

Default features select only `stdio`. Use `default-features = false` when a
different Transport should not carry the stdio dependency graph.

```toml
[dependencies]
lspf = { version = "0.5.2", default-features = false, features = ["tcp"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The crate requires Rust 1.98 or newer. The example selects only the TCP
adapter; keep the default features when using stdio.

| Feature | Default | Enables | Public effect |
| --- | --- | --- | --- |
| `default` | Yes | `stdio` | The native stdio experience |
| `stdio` | Via `default` | `runtime-tokio`, `tokio-util/codec`, `tokio/process` | `stdio`, `StdioBuilder`, stdio Transport types, and stdio child supervision on native targets |
| `tcp` | No | `runtime-tokio`, `tokio-util/codec`, `tokio/net` | `tcp`, `TcpBuilder`, and the TCP Transport types on native targets |
| `websocket` | No | `runtime-tokio`, `tokio-tungstenite`, `tokio/net` | `websocket`, `WebSocketBuilder`, and the WebSocket Transport types on native targets |
| `runtime-tokio` | Through a native Transport | `tokio` | Native execution for `Server::serve`; no I/O adapter by itself |
| `wasm` | No | `wasm-bindgen-futures` | WASM execution for `Server::serve`; no I/O adapter by itself |
| `worker-channel` | No | `wasm`, `js-sys`, `wasm-bindgen`, `web-sys` | `worker_channel` and its `MessagePort` Transport types on `wasm32` |
| `proposed` | No | Nothing else | Draft LSP types and client helpers; independent of every Transport |

Application code also lists dependencies it names directly. For example, a
`#[tokio::main]` binary needs its own `tokio` dependency even though lspf's
selected native Transport uses Tokio internally.

## Target and feature compatibility

| Target and feature selection | Status | Reason or available serving path |
| --- | --- | --- |
| Native default or `stdio` | Supported | Serve with `lspf::stdio(server)` |
| Native `tcp` | Supported | Serve with `lspf::tcp(server, address)` |
| Native `websocket` | Supported | Serve with `lspf::websocket(server, address)` |
| Native `runtime-tokio` without an adapter | Supported for custom Transports | Call `server.serve(custom_transport)` |
| Native with no runtime feature | Supported for protocol-only compilation | Registration and protocol types are available, but serving is not |
| Native `worker-channel` | Intentionally invalid | The feature emits a compile error because `MessagePort` belongs in a WASM Worker |
| `wasm32-unknown-unknown` `worker-channel` | Supported | It implies `wasm`; serve with `lspf::worker_channel(server, port)` |
| `wasm32-unknown-unknown` `wasm` without an adapter | Supported for custom Transports | Call `server.serve(custom_transport)` |
| `wasm32-unknown-unknown` without `wasm` | Intentionally invalid | Every WASM build requires its target runtime glue |
| `wasm32-unknown-unknown` default or `stdio` | Unsupported | stdio is a native adapter; disable default features |
| `wasm32-unknown-unknown` with `tcp` or `websocket` | Intentionally invalid | These adapters require native Tokio sockets and emit a compile error |
| Any supported row plus `proposed` | Supported | `proposed` adds protocol API and selects no Transport or runtime |

Do not combine native adapters with `worker-channel` in one build. A project
that ships native and WASM artifacts selects their features in separate Cargo
commands.

## Buildable examples and shared handlers

The examples keep every handler in
[`examples/shared/mod.rs`](../../crates/lspf/examples/shared/mod.rs). The
same `hover`, `completion`, `shared/ping`, and `didOpen` hook names,
parameters, and return shapes are registered for every host. Only the last
serving call differs:

- [`shared_server.rs`](../../crates/lspf/examples/shared_server.rs) serves the
  shared handler set over stdio and also compiles as a runtime-only WASM
  example.
- [`native_tcp.rs`](../../crates/lspf/examples/native_tcp.rs) serves it over
  one TCP connection.
- [`native_websocket.rs`](../../crates/lspf/examples/native_websocket.rs)
  serves it over one WebSocket connection.
- [`worker_channel.rs`](../../crates/lspf/examples/worker_channel.rs) exports
  a wasm-bindgen `serve(MessagePort)` function for browser and Node Workers.
  Its
  [`browser`](../../crates/lspf/examples/worker_channel_hosts/browser/package.json)
  and
  [`node`](../../crates/lspf/examples/worker_channel_hosts/node/package.json)
  host packages compile that Rust export, generate the appropriate JavaScript
  glue, and validate the host files.

Build the native examples independently so each resolves only its adapter:

```bash
cargo check -p lspf --example native_tcp \
  --no-default-features --features tcp
cargo check -p lspf --example native_websocket \
  --no-default-features --features websocket
```

Build both WASM examples for their real target:

```bash
cargo check -p lspf --example shared_server \
  --target wasm32-unknown-unknown --no-default-features --features wasm
cargo check -p lspf --example worker_channel \
  --target wasm32-unknown-unknown --no-default-features \
  --features worker-channel
```

### Browser Worker host

Install the wasm-bindgen CLI version matching `Cargo.lock`, then build the
checked-in browser host from the repository root:

```bash
cargo install wasm-bindgen-cli --version 0.2.126 --locked
npm --prefix crates/lspf/examples/worker_channel_hosts/browser run build
```

That package runs these exact Rust and wasm-bindgen commands before checking
the host modules with Node's JavaScript parser:

```bash
cargo build -p lspf --example worker_channel \
  --target wasm32-unknown-unknown --no-default-features \
  --features worker-channel --locked
wasm-bindgen --target web \
  --out-dir crates/lspf/examples/worker_channel_hosts/browser/pkg \
  target/wasm32-unknown-unknown/debug/examples/worker_channel.wasm
```

Serve the
[`browser` directory](../../crates/lspf/examples/worker_channel_hosts/browser/index.html)
from an HTTP server and open `index.html`. `main.mjs` creates the channel and
transfers one endpoint; its exported `lspPort` belongs to the LSP client. The
module Worker initializes the generated web binding and passes the transferred
port to the Rust `serve` export.

### Node Worker host

Build and run the checked-in Node host from the repository root:

```bash
npm --prefix crates/lspf/examples/worker_channel_hosts/node run build
npm --prefix crates/lspf/examples/worker_channel_hosts/node run smoke
```

Its build runs the same Cargo command above and generates CommonJS bindings
with this exact command:

```bash
wasm-bindgen --target nodejs \
  --out-dir crates/lspf/examples/worker_channel_hosts/node/pkg \
  target/wasm32-unknown-unknown/debug/examples/worker_channel.wasm
```

[`main.cjs`](../../crates/lspf/examples/worker_channel_hosts/node/main.cjs)
creates a `worker_threads.MessageChannel`, transfers the server port to
[`worker.cjs`](../../crates/lspf/examples/worker_channel_hosts/node/worker.cjs),
and uses the client port to complete initialize, initialized, shutdown, and
exit. The smoke command fails unless the Worker returns a successful
`Outcome`.

The JavaScript host owns Worker creation and termination. The lspf adapter
starts and closes only the supplied port.

## Stdio rules

Stdio is available only with the `stdio` feature on a native target. It is a
binary I/O channel: lspf reads stdin and writes stdout as bytes using the LSP
`Content-Length: N\r\n\r\n` framing contract. Do not print human-readable
output to stdout; configure `tracing` and every other log sink to write only
to stderr.

`lspf::stdio(server).serve().await` returns an `Outcome`. It never terminates
the process. The binary decides whether to call `outcome.code()`, report an
error, restart, or perform other cleanup. Reader EOF, peer closure, protocol
`exit`, and fatal initialization all go through that same outcome path.

### Launching a language-server child

A native Client can own an arbitrary command as one supervised stdio child.
`spawn` replaces all three standard-stream settings with pipes, connects and
initializes the Client, drives incoming protocol traffic, and drains stderr
concurrently:

```rust,no_run
use lspf::types::ClientCapabilities;
use lspf::Client;
use tokio::process::Command;

# async fn run() -> Result<(), lspf::ChildError> {
let command = Command::new("rust-analyzer");
let child = Client::builder(ClientCapabilities::default())
    .spawn(command)
    .await?;
let server = child.server();

// Send typed requests and notifications through `server` while the child is live.
let output = child.shutdown().await?;
assert!(output.status().success());
# Ok(())
# }
```

`shutdown` sends the LSP `shutdown` request and `exit` notification, then
reaps the process. A child that does not exit is escalated through a bounded
wait, terminate, another bounded wait, and kill. `wait` instead observes a
server that exits by itself. Both return the protocol `Outcome`, OS exit
status, and the first 64 KiB of stderr; stderr continues draining after that
capture limit. Dropping a live `ChildConnection` transfers its resources to a
reaper thread and schedules graceful protocol cleanup on the current Tokio
runtime. The thread's synchronous terminate-kill-reap path continues even if
that runtime stops. If no runtime is current when Drop runs, it performs the
same process cleanup synchronously.

## Implementing a custom Transport

Implement `Transport` as two independently owned halves:

- `TransportReader::recv` yields exactly one complete, decoded `RawMessage`
  per call. It must not expose partial bytes or combine envelopes.
- `TransportWriter::send` encodes exactly one `RawMessage`. Calls arrive on
  one writer task and must retain their call order.
- Reads and writes may proceed concurrently because `Transport::split`
  transfers each half to its own protocol-engine task.
- `TransportWriter::shutdown` consumes the writer. Flush any already accepted
  output, send a protocol-specific close when one exists, and release the
  underlying channel.

The adapter owns wire framing and JSON-RPC envelope conversion. A byte-stream
adapter usually adds and strips `Content-Length`; an already message-framed
channel maps one channel message to one `RawMessage`. Enforce a finite message
size before allocating or sending large bodies.

Preserve ordering in each direction. Do not spawn a task per send, reorder
responses around notifications, or deliver messages received after a close.
Return `TransportError::Closed` for ordinary EOF, peer close, or a write to a
gone peer. Use `Malformed` for invalid framing or envelope data,
`OversizedMessage` for a size limit, `Io` when the I/O source is meaningful,
and `Serde` for JSON conversion. The first close cause observed by the engine
wins; a custom adapter should not retry or reconnect behind that boundary.

Call `server.serve(custom_transport)` after constructing the adapter. The
caller supplies any wrapping below it. For example, TLS certificate policy,
mTLS, ALPN, and rotation belong to the application: accept and authenticate a
TLS stream first, then implement the message-framed Transport over that
stream. lspf does not add TLS implicitly.

## Transport scope

The built-in Transport surface does not provide TLS configuration, multi-client
serving, WebSocket client mode, reconnect, CLI Transport selection,
notebook/client frameworks, or shared-memory WASM. These are deployment or
client-framework policies rather than behavior hidden inside one `Server`
connection. Build them outside lspf or implement a custom Transport where the
message-framed contract is sufficient.
