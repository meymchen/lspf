---
title: Drive a language server
description: Build a native LSP client that launches and controls a server over stdio.
---

This tutorial builds a native LSP client for the server from the
[Server tutorial](server). The client launches that server as a supervised
stdio child, receives diagnostics, sends typed notifications and requests, and
shuts the process down cleanly.

`ci/check-tutorials.sh` extracts the manifest and complete program, substitutes
the packaged crate path for the published lspf dependency, and builds both
tutorials as separate Cargo projects. CI then runs this client against the
tutorial server. You need Rust 1.98 or newer.

## 1. Create the crate

```console
cargo new lspf-tutorial-client
cd lspf-tutorial-client
```

Replace `Cargo.toml` with this manifest:

<!-- lspf:tutorial-manifest -->
```toml
[package]
name = "lspf-tutorial-client"
version = "0.1.0"
edition = "2024"

[dependencies]
lspf = "1.0.0"
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "sync", "time"] }
```

The default lspf features include stdio child supervision. Tokio is still a
direct dependency because the program names `tokio::process` and
`tokio::sync` itself.

## 2. Register reverse traffic before connecting

An LSP server can send requests and notifications back to its client. Register
those handlers on `Client::builder` before the connection starts. The tutorial
server publishes diagnostics after `textDocument/didOpen`, so the client
captures that notification in a channel owned by the application:

```rust,no_run
# use lspf::types::ClientCapabilities;
# use lspf::types::notification::PublishDiagnostics;
# use lspf::Client;
let (diagnostics_tx, mut diagnostics_rx) = tokio::sync::mpsc::unbounded_channel();
let client = Client::builder(ClientCapabilities::default())
    .notification::<PublishDiagnostics, _, _>(move |_ctx, params| {
        let _ = diagnostics_tx.send(params);
        async {}
    });
# let _ = (&client, &mut diagnostics_rx);
```

`ClientContext` contains protocol state and a `ServerHandle`; it is not an
editor model. Keep buffers, UI state, filesystem access, and restart policy in
your application and capture the pieces a reverse handler needs.

## 3. Launch the child

`ClientBuilder::spawn` takes a `tokio::process::Command`. It replaces the
command's stdio configuration with protocol pipes, sends `initialize` and
`initialized`, starts the incoming driver, and returns only after the server is
ready:

```rust,no_run
# use lspf::types::ClientCapabilities;
# use lspf::Client;
# async fn launch() -> Result<(), lspf::ChildError> {
let child = Client::builder(ClientCapabilities::default())
    .spawn(tokio::process::Command::new("lspf-tutorial-server"))
    .await?;
let server = child.server();
# let _ = server;
# child.shutdown().await?;
# Ok(())
# }
```

The returned `ChildConnection` owns the protocol driver, process, and stderr
drain. Keep it in the task responsible for shutdown. Clone `server` when other
tasks need to send typed traffic.

## 4. Send typed traffic

`ServerHandle::notify` and `ServerHandle::request` take the protocol marker as
a type parameter. The marker fixes the wire method and the parameter/result
types, so there is no method string or response cast in application code.

This tutorial opens a document, waits for the resulting diagnostics, asks for
hover text, and invokes the Command registered by the server. Notification
sends are synchronous queue admissions; request calls wait for the correlated
response.

## 5. Shut down and inspect the process

`child.shutdown()` performs the whole terminal sequence: send `shutdown`, send
`exit`, wait for protocol completion, and reap the process. It returns an
`Outcome`, OS exit status, and a bounded stderr capture. Use `child.wait()`
instead when the server is expected to exit by itself.

Stderr is drained even after its 64 KiB capture fills, so a noisy child cannot
deadlock. Check `stderr_truncated()` before assuming the captured bytes are the
complete log.

## 6. The complete program

This is `src/main.rs` in full. Pass the server executable as its only argument.

<!-- lspf:tutorial-program -->
```rust,no_run
use std::io;
use std::time::Duration;

use lspf::types::notification::{DidOpenTextDocument, PublishDiagnostics};
use lspf::types::request::{ExecuteCommand, HoverRequest};
use lspf::types::{
    ClientCapabilities, DidOpenTextDocumentParams, ExecuteCommandParams, HoverContents,
    HoverParams, Position, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Uri, WorkDoneProgressParams,
};
use lspf::{Client, Outcome};
use tokio::process::Command;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server_program = std::env::args_os().nth(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: lspf-tutorial-client PATH_TO_LSPF_TUTORIAL_SERVER",
        )
    })?;

    let (diagnostics_tx, mut diagnostics_rx) = tokio::sync::mpsc::unbounded_channel();
    let child = Client::builder(ClientCapabilities::default())
        .notification::<PublishDiagnostics, _, _>(move |_ctx, params| {
            let _ = diagnostics_tx.send(params);
            async {}
        })
        .spawn(Command::new(server_program))
        .await?;
    let server = child.server();

    let uri = "file:///tmp/lspf-tutorial.txt".parse::<Uri>()?;
    server.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: uri.clone(),
            language_id: "plaintext".into(),
            version: 1,
            text: "one two three\nthis line is deliberately longer than forty characters\n".into(),
        },
    })?;

    let published = tokio::time::timeout(Duration::from_secs(5), diagnostics_rx.recv())
        .await?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the server closed before publishing diagnostics",
            )
        })?;
    assert_eq!(published.uri, uri);
    assert_eq!(published.version, Some(1));
    assert_eq!(published.diagnostics.len(), 1);

    let hover = server
        .request::<HoverRequest>(HoverParams {
            work_done_progress_params: WorkDoneProgressParams::default(),
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position::new(0, 0),
            },
        })
        .await?
        .ok_or_else(|| io::Error::other("the server returned no hover"))?;
    match hover.contents {
        HoverContents::MarkupContent(content) => assert_eq!(content.value, "11 words"),
        other => return Err(format!("unexpected hover contents: {other:?}").into()),
    }

    let count = server
        .request::<ExecuteCommand>(ExecuteCommandParams {
            command: "tutorial.countWords".into(),
            arguments: Some(vec![serde_json::json!(uri.as_str())]),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await?;
    assert_eq!(count, Some(serde_json::json!(11)));

    let output = child.shutdown().await?;
    assert_eq!(output.outcome(), Outcome::Exit { code: 0 });
    if !output.status().success() {
        return Err(format!("server exited with {}", output.status()).into());
    }
    if !output.stderr().is_empty() {
        eprintln!("{}", String::from_utf8_lossy(output.stderr()));
    }
    if output.stderr_truncated() {
        eprintln!("server stderr was truncated");
    }
    Ok(())
}
```

Build both tutorial projects, then run:

```console
cargo run -- /absolute/path/to/lspf-tutorial-server
```

No output means every assertion passed. Set `RUST_LOG=lspf=trace` in the
client's environment to have the child emit lspf protocol events on stderr.

## Where to go next

- [Client adoption](../guides/client-adoption) covers custom Transports,
  reverse requests, deadlines, early exits, and connection ownership.
- [Protocol testing](../guides/testing) replaces the child process with a
  deterministic in-memory peer.
- [Errors and cancellation](../guides/errors-and-cancellation) maps the
  endpoint error types and cancellation paths.
- [Resource and observability policies](../guides/operations) covers budgets
  and telemetry; [deployment and troubleshooting](../guides/deployment-and-troubleshooting)
  covers process topology, shutdown, and failure diagnosis.
