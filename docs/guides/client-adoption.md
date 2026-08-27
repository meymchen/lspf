# Adopting the Client endpoint

[English](./client-adoption.md) | [Simplified Chinese](./client-adoption.zh-CN.md)

The [`Client`](lspf::Client) endpoint lets an application connect to a language
server. It works over a caller-provided [`Transport`](lspf::Transport), or it
can launch and supervise a native stdio child. In both cases, lspf owns LSP
correlation and lifecycle state. The application still owns editor behavior:
workspace models, UI, filesystem access, progress presentation, and decisions
about diagnostics or edits.

This guide has two complete walkthroughs. They compile as doctests against the
public crate. The downstream-only journey in
[`public_conformance.rs`](../../crates/lspf/tests/public_conformance.rs) also
runs both connection shapes against a real protocol peer.

## Choose a connection shape

Use a custom Transport when the application already has a message channel or
controls its own process and network lifecycle. Use `ClientBuilder::spawn`
when one Client should own one native language-server process from launch
through reap.

The ownership boundary differs:

| Type | Owns | Clone? |
| --- | --- | --- |
| `Client<T>` | Initialization inputs, reverse-handler registrations, policy, and one Transport before connection | No |
| `ClientConnection` | The initialized generic connection and its inbound protocol driver | No; `serve` consumes it |
| `ServerHandle` | Typed client-to-server calls and lifecycle transitions for that connection | Yes |
| `ClientContext` | One reverse call's request ID, tracing span, and a `ServerHandle` | Yes |
| `ChildConnection` | A `ClientConnection`, child process, protocol driver, and stderr drain | No; `shutdown` or `wait` consumes it |

`ServerHandle` is deliberately smaller than either connection owner. Clone it
for tasks that need to send requests or notifications, but keep exactly one
task responsible for the terminal lifecycle.

## Walkthrough: connect over a custom Transport

This example uses two Tokio channels as an already message-framed Transport.
One side runs a Server only to make the example self-contained. A real host
would replace that side with its existing channel adapter.

```rust,no_run
use std::time::Duration;

use lspf::types::ClientCapabilities;
use lspf::types::request::Request;
use lspf::{
    Client, LspError, Outcome, RawMessage, ResourcePolicy, Server, ServerContext,
    Transport, TransportError, TransportReader, TransportWriter,
};
use tokio::sync::mpsc;

enum Echo {}

impl Request for Echo {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "example/echo";
}

enum Confirm {}

impl Request for Confirm {
    type Params = String;
    type Result = bool;
    const METHOD: &'static str = "example/confirm";
}

type Incoming = Result<RawMessage, TransportError>;

struct ChannelTransport {
    incoming: mpsc::UnboundedReceiver<Incoming>,
    outgoing: mpsc::UnboundedSender<Incoming>,
}

struct ChannelReader(mpsc::UnboundedReceiver<Incoming>);
struct ChannelWriter(mpsc::UnboundedSender<Incoming>);

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            ChannelReader(self.incoming),
            ChannelWriter(self.outgoing),
        )
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Incoming {
        self.0.recv().await.unwrap_or(Err(TransportError::Closed))
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        self.0
            .send(Ok(message))
            .map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

fn channel_pair() -> (ChannelTransport, ChannelTransport) {
    let (to_server, server_incoming) = mpsc::unbounded_channel();
    let (to_client, client_incoming) = mpsc::unbounded_channel();
    (
        ChannelTransport {
            incoming: server_incoming,
            outgoing: to_client,
        },
        ChannelTransport {
            incoming: client_incoming,
            outgoing: to_server,
        },
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (server_transport, client_transport) = channel_pair();

    let language_server = Server::builder(())
        .request::<Echo, _, _>(
            |_state, ctx: ServerContext, text, _cancellation| async move {
                // A Server handler can make a typed reverse request.
                let accepted = ctx
                    .client()
                    .request::<Confirm>(text.clone())
                    .await
                    .map_err(LspError::internal)?;
                Ok(if accepted { text } else { String::new() })
            },
        )
        .build()?;
    let server_task = tokio::spawn(language_server.serve(server_transport));

    let mut policy = ResourcePolicy::default();
    policy.outbound_request_timeout = Some(Duration::from_secs(5));
    policy.handler_timeout = Duration::from_secs(10);

    let client = Client::builder(ClientCapabilities::default())
        .resource_policy(policy)
        .request::<Confirm, _, _>(|ctx, text, cancellation| async move {
            // ClientContext contains protocol state, not an editor model. A
            // nested request can use ctx.server(); UI policy belongs outside.
            let _server = ctx.server();
            tokio::select! {
                _ = cancellation.cancelled() => Err(LspError::RequestCancelled),
                accepted = async move { Ok(!text.is_empty()) } => accepted,
            }
        })
        .build(client_transport)?;

    let connection = client.connect().await?;
    let server = connection.server();
    let client_task = tokio::spawn(connection.serve());

    assert_eq!(server.request::<Echo>("hello".into()).await?, "hello");

    // One task owns the orderly terminal sequence. After shutdown succeeds,
    // only exit or disconnect is valid.
    server.shutdown().await?;
    server.exit()?;
    assert_eq!(client_task.await??, Outcome::Exit { code: 0 });
    assert_eq!(server_task.await??, Outcome::Exit { code: 0 });
    Ok(())
}
```

The adapter must yield one complete `RawMessage` per read, preserve send order,
and return `TransportError::Closed` for ordinary EOF or peer closure. A framing
or I/O failure ends the connection; lspf does not reconnect behind the
application's back. When `ClientConnection::serve` returns, pending typed calls
resolve instead of hanging. Call `ServerHandle::disconnect` if the application
needs to close its side without sending `shutdown` and `exit`.

## Walkthrough: own a stdio language-server child

With the default `stdio` feature, `ClientBuilder::spawn` pipes stdin, stdout,
and stderr, completes initialization, starts the protocol driver and stderr
drain, then returns one `ChildConnection`. The reverse notification handler
below stores raw diagnostics in caller-owned state. Rendering them is still the
editor's job.

```rust,no_run
use std::sync::Arc;
use std::time::Duration;

use lspf::types::notification::{DidOpenTextDocument, PublishDiagnostics};
use lspf::types::{
    ClientCapabilities, DidOpenTextDocumentParams, PublishDiagnosticsParams,
    TextDocumentItem, Uri,
};
use lspf::{Client, ResourcePolicy};
use tokio::process::Command;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let diagnostics = Arc::new(Mutex::new(Vec::<PublishDiagnosticsParams>::new()));
    let diagnostics_for_handler = Arc::clone(&diagnostics);

    let mut policy = ResourcePolicy::default();
    policy.outbound_request_timeout = Some(Duration::from_secs(10));

    let child = Client::builder(ClientCapabilities::default())
        .resource_policy(policy)
        .notification::<PublishDiagnostics, _, _>(move |_ctx, params| {
            let diagnostics = Arc::clone(&diagnostics_for_handler);
            async move {
                diagnostics.lock().await.push(params);
            }
        })
        .spawn(Command::new("rust-analyzer"))
        .await?;

    let server = child.server();
    server.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: "file:///workspace/src/main.rs".parse::<Uri>()?,
            language_id: "rust".into(),
            version: 1,
            text: "fn main() {}\n".into(),
        },
    })?;

    // This consumes the owner, sends shutdown and exit, and reaps the process.
    let output = child.shutdown().await?;
    if !output.stderr().is_empty() {
        eprintln!("{}", String::from_utf8_lossy(output.stderr()));
    }
    if output.stderr_truncated() {
        eprintln!("language-server stderr was truncated");
    }
    if !output.status().success() {
        return Err(format!("language server exited with {}", output.status()).into());
    }
    Ok(())
}
```

Use `shutdown` when the application initiates a graceful stop. Use `wait` when
the child is expected to exit by itself; it returns the same `ChildOutput` with
the protocol `Outcome`, OS status, and captured stderr. An early process exit
closes the connection and resolves pending requests with
`ClientError::Cancelled`. Inspect the status and stderr before deciding whether
to restart or report the failure.

Stderr is always drained to prevent a pipe deadlock. `ChildOutput` retains its
first 64 KiB and records whether the rest was truncated. If graceful shutdown
stalls, supervision uses bounded waits before terminate and kill. Dropping a
live `ChildConnection` also transfers the process to cleanup code that will
reap it, but an explicit `shutdown` or `wait` is better because it returns the
terminal evidence.

## Deadlines and cancellation

`ResourcePolicy` belongs to one Client connection. Its outbound message and
byte limits bound queued calls. `outbound_request_timeout` applies to requests
sent through `ServerHandle`; expiry returns `ClientError::Timeout`, removes the
pending request, and attempts one `$/cancelRequest`. Set it to `None` only when
the application has another firm deadline.

`handler_timeout` bounds reverse request handlers. A peer cancellation or
deadline fires the handler's `CancellationToken`; handlers should stop work
promptly and return `LspError::RequestCancelled` when appropriate. Dropping a
pending request future also removes it and attempts cancellation. Late
responses cannot satisfy later requests because request IDs are never reused.

## Failure and shutdown checklist

- Drive a custom `ClientConnection::serve` concurrently with every use of its
  `ServerHandle`.
- Give one task responsibility for `shutdown` followed by `exit`. Use
  `disconnect` for local teardown when graceful protocol traffic is unwanted.
- Treat Transport failure, EOF, and early child exit as terminal. Pending calls
  resolve during the shared close path.
- Keep editor state and policy in application-owned values captured by reverse
  handlers. `ClientContext` is protocol-only.
- Prefer `ChildConnection::shutdown` or `wait` over Drop so the application can
  inspect `Outcome`, exit status, and stderr.
