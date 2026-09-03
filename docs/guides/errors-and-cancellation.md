# Errors, cancellation, and blocking work

lspf reports failures at the boundary that owns them. A bad registration is a
`BuildError`; a handler chooses an `LspError` response; typed peer operations
return `ClientError`; and serving a connection returns either an `Outcome` or
the terminal `Error` that prevented one. Keeping those paths separate lets an
application decide which failures reach the peer, logs, metrics, or a process
supervisor.

## Choose the right error type

| Boundary | Type | What the caller should do |
| --- | --- | --- |
| `ServerBuilder::build` or `ClientBuilder::build` | `BuildError` | Fix a static registration or resource-policy mistake before serving. |
| Request or Command handler | `LspError` | Return the JSON-RPC/LSP error the peer should receive. |
| `ClientHandle` or `ServerHandle` operation | `ClientError` | Handle lifecycle, overload, timeout, closure, encoding, or remote failure. |
| `ProgressHandle` operation | `ProgressError` through `ClientError::Progress` | Stop using an ended, cancelled, or unknown progress token. |
| `Server::serve` or `ClientConnection::serve` | `lspf::Error` | Treat a Transport or connection-establishment failure as terminal. |
| Completed connection | `Outcome` | Decide the process exit code, restart policy, or cleanup outside lspf. |
| Supervised stdio process | `ChildError` or `ChildOutput` | Distinguish setup or supervision failure from final protocol and OS status. |
| `Workspace::text_document` | `WorkspaceError` | Recover from an unavailable, unsupported, invalid, or oversized resource. |

`BuildError` never goes on the wire. `Outcome` is not an error by itself: it
records `Exit`, `TransportClosed`, `WriterFailed`, or `InitializeFailed` after
the connection has completed cleanup. A server binary can pass
`outcome.code()` to `std::process::exit`; an embedding can inspect the variant
and keep its host process alive.

## Return an LSP error from a handler

The variants map directly to protocol codes:

| `LspError` | Code | Use it when |
| --- | ---: | --- |
| `InvalidParams` | -32602 | The peer's parameters are malformed or fail method-specific validation. |
| `InvalidRequest` | -32600 | The request is valid JSON but invalid in the server's current domain state. |
| `MethodNotFound` | -32601 | A dynamic route cannot serve the method. Ordinary missing registrations are handled by the engine. |
| `Internal` | -32603 | An unexpected local dependency or invariant failed. Avoid secrets in its message. |
| `RequestCancelled` | -32800 | The peer cancelled the work, or the handler cooperatively accepted cancellation. |
| `ContentModified` | -32801 | The document changed and the computed result is now stale. |
| `ServerCancelled` | -32802 | The server stopped work for its own reason and the client may retry. |
| `RequestFailed` | -32803 | The request was valid but could not be completed. |
| `ServerNotInitialized` | -32002 | A request arrived before initialization; the engine normally owns this response. |
| `ServerError` | Application-defined | A private extension needs a stable custom code, message, and optional data value. |

The helper constructors cover common validation failures:

```rust
# use lspf::LspError;
fn parse_limit(raw: &str) -> Result<usize, LspError> {
    raw.parse()
        .map_err(|error| LspError::invalid_params(format!("invalid limit: {error}")))
}

fn missing_document(uri: &str) -> LspError {
    LspError::invalid_request(format!("document is not open: {uri}"))
}
```

Do not turn expected absence into an error when the LSP result type already has
an empty form. A hover handler normally returns `Ok(None)` when it has nothing
to show.

## Request cancellation

Every request and Command handler receives a request-scoped
`CancellationToken`. A peer `$/cancelRequest`, successful connection shutdown,
or handler deadline cancels that token. The engine's completion gate chooses
exactly one response, so a late handler result cannot race a cancellation into
a second response.

Awaiting async I/O is naturally cooperative. Use `tokio::select!` when the
operation does not already accept a token:

```rust,no_run
# use lspf::{CancellationToken, LspError};
# async fn remote_lookup() -> Result<String, std::io::Error> { Ok(String::new()) }
async fn cancellable_lookup(cancellation: CancellationToken) -> Result<String, LspError> {
    tokio::select! {
        _ = cancellation.cancelled() => Err(LspError::RequestCancelled),
        result = remote_lookup() => result.map_err(LspError::internal),
    }
}
```

CPU work does not yield just because its handler is async. Split it into bounded
units and check `is_cancelled()` between units. Return promptly; cancellation is
cooperative on native and WASM targets.

Handler deadline expiry also cancels the token, but the engine returns
`ServerCancelled` with the stable message `handler deadline expired`. Returning
`RequestCancelled` yourself is appropriate when your code observes a peer or
application cancellation before the deadline wins.

For outbound requests, dropping the pending future removes its correlation
entry and attempts one `$/cancelRequest`. The connection's
`outbound_request_timeout` does the same and returns `ClientError::Timeout`.
A peer may still finish the remote work; late responses are ignored and request
IDs are not reused.

## Detect stale document work

lspf cannot infer which document a user handler's result depends on. Capture
the input version and compare it with the current retained snapshot before
returning a result:

```rust
# use lspf::{LspError, ServerContext};
# use lspf::types::Uri;
fn reject_stale(ctx: &ServerContext, uri: &Uri, started_at: Option<i32>) -> Result<(), LspError> {
    let current = ctx.documents().get(uri).and_then(|document| document.version());
    if current != started_at {
        return Err(LspError::ContentModified);
    }
    Ok(())
}
```

Compare after expensive work and before constructing the final response. For a
provider-loaded snapshot, `version()` is `None`; application-owned content
hashes or generations are a better stale-work key in that case.

## Move blocking work off the executor

Use `tokio::task::spawn_blocking` for filesystem libraries, parsers, or native
APIs that block a thread. Once a blocking closure starts, dropping its join
handle does not stop it. Pass a cloned cancellation token into the closure and
check it between bounded work units:

```rust,no_run
# use lspf::{CancellationToken, LspError};
async fn analyze(cancellation: CancellationToken) -> Result<usize, LspError> {
    let worker_cancellation = cancellation.clone();
    let worker = tokio::task::spawn_blocking(move || {
        let mut completed = 0;
        for _chunk in 0..100 {
            if worker_cancellation.is_cancelled() {
                return None;
            }
            // Run one bounded unit of blocking analysis here.
            completed += 1;
        }
        Some(completed)
    });

    match worker.await.map_err(LspError::internal)? {
        Some(completed) => Ok(completed),
        None => Err(LspError::RequestCancelled),
    }
}
```

Limit the blocking pool or put a semaphore around expensive jobs when the
runtime's shared pool is too broad for the workload. lspf's inbound budget
limits admitted protocol requests, not the threads or child processes an
application creates inside those handlers.

The runnable [`blocking_work` example](../../crates/lspf/examples/blocking_work.rs)
shows blocking work alongside an unrelated completion request.

## Observe failures without exposing payloads

`ServerBuilder::on_error` receives connection failure categories and
non-sensitive identity outside the user Layer chain. It covers framing,
protocol, Transport, panic isolation, overload, and close failures. Reports do
not contain parameters, results, document text, wire data, panic payloads, or
underlying error messages.

Use the hook for counters and alerts. Use ordinary application logging where a
handler owns a concrete failure and can redact it correctly. A panic in the
hook is isolated and cannot alter cleanup or the selected `Outcome`.
