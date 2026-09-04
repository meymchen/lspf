---
title: Build a language server
description: Build a complete stdio language server, then drive it from a terminal or editor.
---

This tutorial builds a complete stdio language server from an empty Cargo
project. The finished server answers `textDocument/hover` with a word count,
publishes a diagnostic for every over-long line as the document changes, and
exposes one Command the editor can invoke by name.

Nothing here is a snippet in isolation: `ci/check-tutorials.sh` extracts the
manifest and complete program, substitutes the packaged crate path for the
published lspf dependency, and compiles them in a fresh directory. The Client
tutorial then drives a real LSP lifecycle. What you read is what CI runs.

You need Rust 1.98 or newer. An editor is optional; the last step drives the
server from a terminal.

## 1. Create the crate

```console
cargo new lspf-tutorial-server
cd lspf-tutorial-server
```

Replace `Cargo.toml` with this manifest. `lspf` brings the default `stdio`
feature, which also selects the Tokio runtime that serves the connection.
Every crate the program names appears here, including the ones lspf uses
internally.

<!-- lspf:tutorial-manifest -->
```toml
[package]
name = "lspf-tutorial-server"
version = "0.1.0"
edition = "2024"

[dependencies]
lspf = "1.0.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

## 2. Hold only your own state

The framework owns the synchronized documents, the workspace, and the client
connection, and hands them to every handler through a [`ServerContext`]
parameter. Your struct therefore holds only what the framework does not: in
this server, the line width that counts as too long.

```rust
struct State {
    max_line_width: usize,
}
```

Handlers receive it as `Arc<State>`, so it is shared, never mutated in place.
A server that needs mutable state puts a lock or a concurrent map inside the
struct; it never stores a `ServerContext`, a `DocumentsView`, or a
`ClientHandle` there, because those belong to one connection and reach the
handler as arguments already.

## 3. Answer a request

A typed feature is one registration. The descriptor
[`lspf::features::hover()`] fixes three things at once: the wire method, the
handler's parameter and result types, and the `hoverProvider` capability the
server will advertise. There is no `ServerCapabilities` literal to keep in
sync.

```rust
# use std::sync::Arc;
# use lspf::types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};
# use lspf::{CancellationToken, LspError, ServerContext};
# struct State { max_line_width: usize }
async fn hover(
    _state: Arc<State>,
    ctx: ServerContext,
    params: HoverParams,
    _cancellation: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let uri = &params.text_document_position_params.text_document.uri;
    // The engine applied `didOpen` and every `didChange` before dispatching
    // this request, so the retained document is the one the editor sees.
    let Some(document) = ctx.documents().get(uri) else {
        return Ok(None);
    };
    let words = document.text().split_whitespace().count();
    Ok(Some(Hover {
        contents: HoverContents::MarkupContent(MarkupContent {
            kind: MarkupKind::PlainText,
            value: format!("{words} words"),
        }),
        range: None,
    }))
}
```

Returning `Ok(None)` for an unknown URI is deliberate. A handler returns
[`LspError`] only for a failure the editor should see as an error response;
"nothing to show here" is a successful empty result.

## 4. React to document changes

`textDocument/didOpen` and `textDocument/didChange` are protocol built-ins:
the engine decodes them and updates its documents itself. Registering a
handler for one of those methods therefore records a *post-mutation hook*: it
observes the document the framework has already updated, and cannot replace
the update.

```rust
# use std::sync::Arc;
# use lspf::types::{
#     Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams,
#     DidOpenTextDocumentParams, Position, PublishDiagnosticsParams, Range, Uri,
# };
# use lspf::{PositionEncoding, ServerContext};
# struct State { max_line_width: usize }
async fn on_did_open(state: Arc<State>, ctx: ServerContext, params: DidOpenTextDocumentParams) {
    publish_line_diagnostics(&state, &ctx, params.text_document.uri);
}

async fn on_did_change(state: Arc<State>, ctx: ServerContext, params: DidChangeTextDocumentParams) {
    publish_line_diagnostics(
        &state,
        &ctx,
        params.text_document.text_document_identifier.uri,
    );
}

fn publish_line_diagnostics(state: &State, ctx: &ServerContext, uri: Uri) {
    let Some(document) = ctx.documents().get(&uri) else {
        return;
    };
    let encoding = ctx.documents().position_encoding();
    let text = document.text();
    let diagnostics = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.chars().count() > state.max_line_width)
        .map(|(number, line)| Diagnostic {
            range: line_range(number, line, encoding),
            severity: Some(DiagnosticSeverity::Warning),
            source: Some("lspf-tutorial-server".into()),
            message: format!("line is longer than {} characters", state.max_line_width).into(),
            ..Diagnostic::default()
        })
        .collect();

    // Publishing is a notification: encoded and enqueued synchronously, with a
    // closing connection as the only ordinary failure.
    if let Err(error) = ctx.publish_diagnostics(PublishDiagnosticsParams {
        uri,
        // The retained version, not the one on the wire: that is the revision
        // these diagnostics actually describe.
        version: document.version(),
        diagnostics,
    }) {
        tracing::warn!(%error, "publishing diagnostics failed");
    }
}

/// `Position.character` counts units of the encoding the connection
/// negotiated, so the end column depends on it.
fn line_range(number: usize, line: &str, encoding: PositionEncoding) -> Range {
    let width = match encoding {
        PositionEncoding::Utf8 => line.len(),
        PositionEncoding::Utf32 => line.chars().count(),
        PositionEncoding::Utf16 => line.encode_utf16().count(),
    };
    let line_index = u32::try_from(number).unwrap_or(u32::MAX);
    Range {
        start: Position {
            line: line_index,
            character: 0,
        },
        end: Position {
            line: line_index,
            character: u32::try_from(width).unwrap_or(u32::MAX),
        },
    }
}
```

`ServerContext::publish_diagnostics` is the short form of
`ctx.client().notify::<PublishDiagnostics>(…)`. For the rest of the
server-to-client surface, including window messages, workspace edits, progress,
and refreshes, see the
[outgoing client helpers](../../guides/outgoing-client/) guide.

## 5. Expose a Command

A Command is a user closure dispatched by name beneath
`workspace/executeCommand`. Its whole `arguments` array is decoded into the
handler's argument type, and registering it adds the name to the generated
`executeCommandProvider` in registration order.

```rust
# use std::str::FromStr;
# use std::sync::Arc;
# use lspf::types::Uri;
# use lspf::{CancellationToken, LspError, ServerContext};
# struct State { max_line_width: usize }
async fn count_words(
    _state: Arc<State>,
    ctx: ServerContext,
    args: Vec<String>,
    _cancellation: CancellationToken,
) -> Result<usize, LspError> {
    let Some(argument) = args.into_iter().next() else {
        return Err(LspError::invalid_params(
            "tutorial.countWords expects one document URI",
        ));
    };
    let uri = Uri::from_str(&argument)
        .map_err(|error| LspError::invalid_params(format!("invalid URI: {error}")))?;
    let Some(document) = ctx.documents().get(&uri) else {
        return Err(LspError::invalid_request(format!(
            "`{}` is not open",
            uri.as_str()
        )));
    };
    Ok(document.text().split_whitespace().count())
}
```

Bad input from the editor is `invalid_params`; a well-formed request the
server cannot satisfy is `invalid_request`. Both become JSON-RPC error
responses with the codes LSP prescribes, and neither ends the connection. The
[errors and cancellation](../../guides/errors-and-cancellation/) guide covers
the rest of the taxonomy.

## 6. Build the server and serve it

Registration order does not matter, and the capabilities follow from the
registrations themselves. `build()` performs no I/O: it returns a
[`BuildError`] for a duplicate method, an empty Command name, or conflicting
capability contributions before any transport exists.

Two rules matter for a stdio server:

- **stdout carries the protocol and nothing else.** Send logs to stderr. A
  stray `println!` corrupts the message stream.
- **serving reports how the connection ended.** `serve` resolves to an
  [`Outcome`]; turning it into a process disposition is the binary's decision,
  not the framework's.

## 7. The complete program

This is `src/main.rs` in full.

<!-- lspf:tutorial-program -->
```rust,no_run
use std::str::FromStr;
use std::sync::Arc;

use lspf::types::notification::{DidChangeTextDocument, DidOpenTextDocument};
use lspf::types::{
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidOpenTextDocumentParams, Hover,
    HoverContents, HoverParams, MarkupContent, MarkupKind, Position, PublishDiagnosticsParams,
    Range, Uri,
};
use lspf::{CancellationToken, LspError, PositionEncoding, Server, ServerContext};

/// Only this server's own state: the framework owns the documents, the
/// workspace, and the client, and passes them to handlers through
/// `ServerContext`.
struct State {
    max_line_width: usize,
}

/// `textDocument/hover`, registered through the sealed `hover()` descriptor
/// that also contributes `hoverProvider` to the generated capabilities.
async fn hover(
    _state: Arc<State>,
    ctx: ServerContext,
    params: HoverParams,
    _cancellation: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let uri = &params.text_document_position_params.text_document.uri;
    let Some(document) = ctx.documents().get(uri) else {
        return Ok(None);
    };
    let words = document.text().split_whitespace().count();
    Ok(Some(Hover {
        contents: HoverContents::MarkupContent(MarkupContent {
            kind: MarkupKind::PlainText,
            value: format!("{words} words"),
        }),
        range: None,
    }))
}

/// A post-mutation hook: the engine has already opened the document.
async fn on_did_open(state: Arc<State>, ctx: ServerContext, params: DidOpenTextDocumentParams) {
    publish_line_diagnostics(&state, &ctx, params.text_document.uri);
}

/// The same hook for incremental edits, after they have been applied.
async fn on_did_change(state: Arc<State>, ctx: ServerContext, params: DidChangeTextDocumentParams) {
    publish_line_diagnostics(
        &state,
        &ctx,
        params.text_document.text_document_identifier.uri,
    );
}

fn publish_line_diagnostics(state: &State, ctx: &ServerContext, uri: Uri) {
    let Some(document) = ctx.documents().get(&uri) else {
        return;
    };
    let encoding = ctx.documents().position_encoding();
    let text = document.text();
    let diagnostics = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.chars().count() > state.max_line_width)
        .map(|(number, line)| Diagnostic {
            range: line_range(number, line, encoding),
            severity: Some(DiagnosticSeverity::Warning),
            source: Some("lspf-tutorial-server".into()),
            message: format!("line is longer than {} characters", state.max_line_width).into(),
            ..Diagnostic::default()
        })
        .collect();

    if let Err(error) = ctx.publish_diagnostics(PublishDiagnosticsParams {
        uri,
        version: document.version(),
        diagnostics,
    }) {
        tracing::warn!(%error, "publishing diagnostics failed");
    }
}

/// `Position.character` counts units of the negotiated encoding.
fn line_range(number: usize, line: &str, encoding: PositionEncoding) -> Range {
    let width = match encoding {
        PositionEncoding::Utf8 => line.len(),
        PositionEncoding::Utf32 => line.chars().count(),
        PositionEncoding::Utf16 => line.encode_utf16().count(),
    };
    let line_index = u32::try_from(number).unwrap_or(u32::MAX);
    Range {
        start: Position {
            line: line_index,
            character: 0,
        },
        end: Position {
            line: line_index,
            character: u32::try_from(width).unwrap_or(u32::MAX),
        },
    }
}

/// A typed Command dispatched by name beneath `workspace/executeCommand`.
async fn count_words(
    _state: Arc<State>,
    ctx: ServerContext,
    args: Vec<String>,
    _cancellation: CancellationToken,
) -> Result<usize, LspError> {
    let Some(argument) = args.into_iter().next() else {
        return Err(LspError::invalid_params(
            "tutorial.countWords expects one document URI",
        ));
    };
    let uri = Uri::from_str(&argument)
        .map_err(|error| LspError::invalid_params(format!("invalid URI: {error}")))?;
    let Some(document) = ctx.documents().get(&uri) else {
        return Err(LspError::invalid_request(format!(
            "`{}` is not open",
            uri.as_str()
        )));
    };
    Ok(document.text().split_whitespace().count())
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    // Logs go to stderr: stdout carries the LSP wire protocol and nothing else.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let server = Server::builder(State { max_line_width: 40 })
        .feature(lspf::features::hover(), hover)
        .command("tutorial.countWords", count_words)
        .notification::<DidOpenTextDocument, _, _>(on_did_open)
        .notification::<DidChangeTextDocument, _, _>(on_did_change)
        .build()
        .expect("the static registrations are valid");

    // Serving reports how the connection ended; this binary decides what that
    // means for the process.
    let outcome = lspf::stdio(server).serve().await?;
    std::process::exit(outcome.code());
}
```

## 8. Drive it from a terminal

Complete the sibling [Client tutorial](../client/), then run its client
against this server. The client waits for each response before it sends
`shutdown`, so shutdown cannot cancel the hover request it is checking:

```bash
cargo build
server="$(pwd)/target/debug/lspf-tutorial-server"
[[ -x "${server}.exe" ]] && server="${server}.exe"
cargo run --manifest-path ../lspf-tutorial-client/Cargo.toml -- "$server"
```

The client asserts that initialization succeeds, diagnostics arrive, hover
reports `11 words`, the Command returns `11`, and the shutdown outcome is a
successful `exit`. The `initialize` response also contains
`"hoverProvider":true` and `"executeCommandProvider"` with
`tutorial.countWords`; both are derived from registrations rather than written
by hand.

Set `RUST_LOG=lspf=trace` to watch the same exchange as structured events on
stderr.

## 9. Point an editor at it

`cargo install --path .` puts `lspf-tutorial-server` on your `PATH`. From
there, any editor that can launch a generic LSP server over stdio can use it;
configure that editor to run `lspf-tutorial-server` for plain-text documents.
For a ready-made VS Code extension alongside the Rust server, start from
[`lspf-vscode-extension-template`](https://github.com/meymchen/lspf-vscode-extension-template)
and move this tutorial's handlers into the generated server.

## Where to go next

- [Drive this server from your own application](../client/): the Client
  tutorial connects to the binary you just built.
- [Feature registration](../../guides/features-and-workspace/) covers the rest
  of the registration surface; [workspace state](../../guides/workspace-state/)
  covers multi-root state, notebooks, and `FileProvider` configuration.
- [Protocol testing](../../guides/testing/): run this server in-process
  against a scripted peer instead of a terminal.
- [Errors and cancellation](../../guides/errors-and-cancellation/): what to
  return, what cancels, and where blocking work goes.
- [Resource and observability policies](../../guides/operations/): budgets,
  concurrency, and logging. Continue to
  [deployment and troubleshooting](../../guides/deployment-and-troubleshooting/)
  for process topology and shutdown.

[`BuildError`]: https://docs.rs/lspf/latest/lspf/enum.BuildError.html
[`LspError`]: https://docs.rs/lspf/latest/lspf/enum.LspError.html
[`Outcome`]: https://docs.rs/lspf/latest/lspf/enum.Outcome.html
[`ServerContext`]: https://docs.rs/lspf/latest/lspf/struct.ServerContext.html
[`lspf::features::hover()`]: https://docs.rs/lspf/latest/lspf/features/fn.hover.html
