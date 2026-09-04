---
title: Use stdio and custom transports
description: Apply stdio ownership rules or implement a message-framed transport for an embedded host.
---

After choosing a connection shape, use this guide for the ownership details of stdio and for hosts that need their own message-framed transport.

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
