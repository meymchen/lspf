# lspf

[![crates.io](https://img.shields.io/crates/v/lspf.svg)](https://crates.io/crates/lspf)
[![docs.rs](https://docs.rs/lspf/badge.svg)](https://docs.rs/lspf)
[![License: MIT OR Apache-2.0](https://img.shields.io/crates/l/lspf)](#license)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/meymchen/lspf)

A Rust framework for building extensible LSP (Language Server Protocol) language servers.

[Project documentation](https://lspf.dev) ·
[简体中文文档](https://lspf.dev/zh-cn/)

`lspf` is **async-only** and designed so a developer can stand up a working
language server in very little code. You register typed handlers on a
`Server`, hand it to a transport, and the framework owns the protocol:
lifecycle, document synchronization, cancellation, bounded concurrency,
`tracing` spans, and typed server-to-client traffic through `ClientHandle`.

> **Status:** the current published release is **1.0.0**, with a frozen public
> interface. It includes the complete stable LSP 3.18 inbound feature catalog,
> UTF-8/UTF-32/UTF-16 position negotiation, notebook synchronization,
> partial-result reporting, all seven workspace refresh requests, typed
> Commands, and the multi-root `Workspace`. Native stdio, TCP, WebSocket, and
> WASM worker-channel transports are available. See the
> [package changelog](./crates/lspf/CHANGELOG.md) for release history.

## Quick start

```rust,no_run
use std::sync::Arc;

use lspf::types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Hover, HoverContents, HoverParams, MarkupContent, MarkupKind,
};
use lspf::{CancellationToken, ServerContext, LspError, OsFileProvider, Server};

/// Only your own application state — the framework owns the documents, the
/// workspace, and the client, and hands them to handlers through `ServerContext`.
struct State;

/// A standard typed feature. The `features::hover()` descriptor fixes the
/// wire method, this handler's parameter and result types, and the
/// `hoverProvider` capability the server will advertise — all at once.
async fn hover(
    _state: Arc<State>,
    ctx: ServerContext,
    params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let Some(document) = ctx.documents().get(&uri) else {
        return Ok(None);
    };
    Ok(Some(Hover {
        contents: HoverContents::MarkupContent(MarkupContent {
            kind: MarkupKind::PlainText,
            value: format!("{} words", document.text().split_whitespace().count()),
        }),
        range: None,
    }))
}

/// A second typed feature; the options supplied here are exactly what the
/// generated `completionProvider` advertises.
async fn complete(
    _state: Arc<State>,
    _ctx: ServerContext,
    _params: CompletionParams,
    _ct: CancellationToken,
) -> Result<Option<CompletionResponse>, LspError> {
    Ok(Some(CompletionResponse::CompletionItemList(vec![CompletionItem {
        label: "hello".into(),
        kind: Some(CompletionItemKind::Text),
        ..CompletionItem::default()
    }])))
}

/// A typed Command, dispatched by name beneath `workspace/executeCommand`.
/// Registering it adds the name to the generated `executeCommandProvider`,
/// in registration order.
async fn roots(
    _state: Arc<State>,
    ctx: ServerContext,
    _args: Vec<String>,
    _ct: CancellationToken,
) -> Result<Vec<(String, String)>, LspError> {
    Ok(ctx
        .workspace()
        .roots()
        .into_iter()
        .map(|folder| (folder.uri.as_str().to_string(), folder.name))
        .collect())
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    // Logs go to stderr: stdout carries the LSP wire protocol and nothing else.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let server = Server::builder(State)
        // Unopened `file:` URIs resolve from disk through this provider.
        .file_provider(OsFileProvider::new())
        .feature(lspf::features::hover(), hover)
        .feature(
            lspf::features::completion(CompletionOptions {
                trigger_characters: Some(vec![".".to_string()]),
                ..CompletionOptions::default()
            }),
            complete,
        )
        .command("hello.roots", roots)
        .build()
        .expect("the static registrations are valid");
    // Serving reports how the connection ended; the binary decides what that
    // means for the process.
    let outcome = lspf::stdio(server).serve().await?;
    std::process::exit(outcome.code());
}
```

No handwritten `ServerCapabilities` and no framework change are involved:
the capabilities come from the registrations themselves. For a ready-to-copy
server and VS Code extension, start from
[`lspf-vscode-extension-template`](https://github.com/meymchen/lspf-vscode-extension-template).
The framework's public feature, workspace, client, and stdio seams are covered
by the `crates/lspf` integration tests and the in-tree `lspf-markdown`
reference server. The
[feature registration](https://lspf.dev/guides/features-and-workspace)
and [workspace state](https://lspf.dev/guides/workspace-state)
guides walk through each piece. To choose a Transport adapter, enable only
its Cargo dependencies, or implement another message-framed channel, see
[Choosing a Transport](https://lspf.dev/guides/transports), then
continue to [stdio and custom transports](https://lspf.dev/guides/stdio-and-custom-transports)
when the host owns a child process or a custom message-framed channel.
To build the same server one step at a time, start with the
[Server tutorial](https://lspf.dev/tutorials/server), then use the
[Client tutorial](https://lspf.dev/tutorials/client) to drive it as a supervised
stdio child. For custom Client Transports and reverse handlers, follow the
[Client adoption guide](https://lspf.dev/guides/client-adoption).
Runnable servers for individual LSP features are indexed in
[`crates/lspf/examples/README.md`](./crates/lspf/examples/README.md).

## Install

```toml
[dependencies]
lspf = "1.0.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

The crate requires Rust 1.98 or newer. The quickstart uses the default `stdio`
feature; select a different feature set for another Transport.

List every crate your application names directly. In this example, `tokio`,
`tracing`, and `tracing-subscriber` are direct dependencies even though lspf
also uses them internally.

## Why lspf

- **Async-first.** The framework is `async fn` end to end; no `tower::Layer`
  interop, no sync escape hatch.
- **Smallest viable server.** Register your handlers on `Server::builder`,
  hand the built `Server` to `lspf::stdio(...)`, and you have a working LSP
  server.
- **Framework-owned document state.** Incremental text changes are applied
  to the concurrency-safe, rope-backed `Documents` the framework owns before
  your hook runs; handlers read them through a `DocumentsView` that has no
  mutation operation.
- **A multi-root `Workspace`.** Client announcements — folders, root URI,
  configuration, trace level — live in one cloneable handle, mutated only by
  the protocol and read through `ServerContext`; unopened files resolve through a
  configurable `FileProvider`.
- **Capabilities that cannot drift.** `ServerCapabilities` are generated from
  the same registrations that dispatch, so what the server advertises is what
  it serves; conflicting registrations are build errors, never silent
  last-write-wins.
- **Safe concurrent dispatch.** Requests and notifications run with a
  configurable concurrency limit (64 by default); `$/cancelRequest`
  propagates through a `CancellationToken`.
- **Protocol details handled for you.** Lifecycle ordering, JSON-RPC framing,
  text synchronization, and UTF-8/UTF-32/UTF-16 position negotiation are built
  in.
- **First-party and custom transports.** `stdio`, single-client TCP,
  single-client WebSocket, and WASM worker-channel adapters are provided;
  implement the public `Transport` traits to embed lspf in tests or another
  message channel.

## Concepts

The vocabulary below is taken from [`CONTEXT.md`](./CONTEXT.md); the
project deliberately standardizes on these terms in the public API and
the docs.

| Term                | Meaning                                                                                                  |
| ------------------- | -------------------------------------------------------------------------------------------------------- |
| `Server`            | Owns exactly one LSP connection; built by `Server::builder(state)` and served over a `Transport`.        |
| Handler             | An async function registered for one LSP method. User handlers take priority over the built-ins.         |
| Built-in handler    | A handler the framework ships. Lifecycle, document sync, and cancellation are protocol built-ins.        |
| Post-mutation hook  | What registering a built-in document notification records: it observes the mutation, never replaces it.  |
| `Command`           | A user closure dispatched by name on `workspace/executeCommand`.                                         |
| `Document`          | A text resource tracked by the framework: URI, language id, version, and rope-backed contents.           |
| `DocumentsView`     | The read-only document handle a handler reaches through `ctx.documents()`.                               |
| `NotebooksView`     | The read-only notebook handle a handler reaches through `ctx.notebooks()`: structure, not cell text.     |
| `Workspace`         | The cloneable handle to the connection's workspace state: folders, configuration, and documents.         |
| `FileProvider`      | The configurable resolver for resources that are not open in the editor.                                 |
| `ServerContext`     | The cheap-to-clone framework-state handle every handler receives: documents, workspace, client, scope.   |
| `ClientHandle`      | The typed handle for server-to-client notifications and requests (`ctx.client()`).                       |
| `PartialResultSink` | The request-scoped typed sink for result chunks, lent by `ctx.partial_results()`.                        |
| `Client`            | Configures one outbound LSP connection over a caller-provided Transport or supervised stdio child.       |
| `ClientConnection`  | Owns one initialized generic Client connection and its inbound protocol driver.                          |
| `ClientContext`     | The protocol-only context passed to reverse handlers; editor state remains caller-owned.                 |
| `ServerHandle`      | The cloneable handle for typed client-to-server calls and Client lifecycle transitions.                  |
| `ChildConnection`   | Owns one initialized stdio Client connection and its supervised language-server process.                 |
| `CancellationToken` | The cancellation signal passed to request handlers.                                                      |
| `Transport`         | A message-framed channel split into reader and writer halves for the protocol engine.                    |
| `Outcome`           | How one connection ended, returned by serving; it carries the LSP exit code but never exits the process. |

## Architecture

The [project documentation](https://lspf.dev) covers
features, transports, testing, operations, and both endpoint tutorials. The
repository keeps the material needed to maintain those contracts:

- [`CONTEXT.md`](./CONTEXT.md) defines the domain language.
- [`docs/adr/`](./docs/adr/) records architecture decisions.
- [`docs/public-interface.md`](./docs/public-interface.md) freezes the 1.0
  public interface.
- [`SECURITY.md`](./SECURITY.md) defines supported platforms, compatibility,
  and vulnerability reporting.

See [`CONTRIBUTING.md`](./CONTRIBUTING.md) before changing these boundaries.

## Current scope

The [support contract](./SECURITY.md) is authoritative for maintained Rust
versions, hosts, targets, and Cargo feature combinations. The
[operations guide](https://lspf.dev/guides/operations#known-limitations) records the
deployment and application responsibilities that lspf deliberately leaves to
its host.

Available today:

- `stdio`, single-client TCP, WebSocket, and WASM worker-channel adapters, plus
  the public custom-transport interface.
- A typed Client endpoint over custom Transport and supervised stdio child,
  with reverse handlers, bounded resources, deadlines, and lifecycle control.
- The built `Server`: typed requests, notifications, commands, the sealed
  feature catalog covering the complete stable LSP 3.18 surface, user `Layer`s, and
  the one `configure_initialize` transaction. The 3.18 additions are
  `textDocument/inlineCompletion`, `workspace/textDocumentContent`, and
  `textDocument/rangesFormatting`.
- Lifecycle hooks through shutdown and exit, incremental or full text-document
  synchronization, and post-mutation document hooks.
- Notebook document synchronization across
  `notebookDocument/didOpen`, `didChange`, `didSave`, and `didClose`, with
  notebook structure read through `ctx.notebooks()` and cell text through the
  same `ctx.documents()` view as any other document.
- The multi-root `Workspace`, latest configuration settings, and
  `FileProvider`-backed unopened-file lookup.
- Typed server-to-client notifications and correlated requests through
  `ClientHandle`, including all seven stable workspace refresh requests.
- Partial-result reporting: a handler for a request that carries
  a `partialResultToken` reports typed chunks through `ctx.partial_results()`
  under the connection's outbound budget.
- Concurrent dispatch, bounded concurrency, request cancellation, and
  `tracing` spans.
- Rope-backed documents with UTF-8/UTF-32/UTF-16 position negotiation.

## Examples

Transport-specific examples reuse one shared handler module, demonstrating
that business logic does not fork between native and WASM hosts. See the
[Transport guide](https://lspf.dev/guides/transports#buildable-examples-and-shared-handlers)
for native TCP, native WebSocket, and browser/Node worker-channel build
commands.

Generate a new Rust language server plus VS Code extension from the dedicated
template repository:

```bash
cargo generate --git https://github.com/meymchen/lspf-vscode-extension-template
```

You can also use GitHub's **Use this template** action or clone the repository
directly. Keeping that starter outside this workspace lets it carry standalone
project metadata, release automation, and editor packaging without coupling
those choices to the framework repository.

For a maintained, task-focused server rather than a template, install the
first-party Markdown link server:

```bash
cargo install --path crates/lspf-markdown
```

The resulting `lspf-markdown` binary diagnoses missing local link targets after
open and incremental edits, describes resolved targets on hover, and navigates
definitions to the target document's first heading. Its public-Transport test
journeys live in
[`crates/lspf-markdown/tests/server_journey.rs`](./crates/lspf-markdown/tests/server_journey.rs).

## Editor setup

This repository is a Cargo workspace with two members:

- [`crates/lspf`](./crates/lspf) — the framework library you depend on
  (`lspf = "1.0.0"`).
- [`crates/lspf-markdown`](./crates/lspf-markdown) — the installable
  **reference server**. It builds the `lspf-markdown` stdio binary and applies
  the framework's public document, workspace, feature, client, and testing
  interfaces to real Markdown link behavior.

Use [`lspf-vscode-extension-template`](https://github.com/meymchen/lspf-vscode-extension-template)
to create a new project. Keep `lspf-markdown` for framework development and
integration validation: unlike a starter, it implements one concrete domain
and must keep working as `lspf` evolves.

### Install the reference server

```bash
cargo install --path crates/lspf-markdown
```

This installs the `lspf-markdown` binary into Cargo's bin directory
(`~/.cargo/bin` by default). Make sure that directory is on your `PATH` so
your editor can launch the server by name.

### Editor validation

The bundled VS Code test client now launches `lspf-markdown` by default. The
same installed binary is also exercised by the checked-in Neovim and Zed
adapters. Follow [`editor-validation/README.md`](./editor-validation/README.md)
for the reproducible three-editor journey, or use the VS Code tasks described
in [`CONTRIBUTING.md`](./CONTRIBUTING.md#debug-a-server).

### Troubleshooting

- **`lspf-markdown` not found / "command not found".** The binary isn't on your
  `PATH`. Confirm `which lspf-markdown` resolves; if not, add `~/.cargo/bin` to
  your `PATH`, or use an absolute path in the editor configuration.
- **The server doesn't start or an expected feature is unavailable.** Make
  sure you ran `cargo install --path crates/lspf-markdown` after your latest
  changes, and that your editor client routes Markdown files to this server.
- **Edited the config but nothing changed.** Editors read LSP settings at
  startup — reload the window after editing `settings.json` (VS Code:
  *Developer: Reload Window*; Zed: reopen the workspace).
- **A direct run appears stuck.** `lspf-markdown` and the framework examples
  speak LSP over stdio. Use one of the VS Code client configurations to start
  the process, then attach CodeLLDB from VS Code or Zed. Use the quick test
  task for an automated check.

## Contributing

Read [`CONTRIBUTING.md`](./CONTRIBUTING.md) for setup, project context, tests,
documentation rules, debugging, and pull request conventions. Issues live on
the [GitHub tracker](https://github.com/meymchen/lspf/issues).

## License

Dual-licensed under either of

- [Apache License, Version 2.0](./LICENSE-APACHE)
- [MIT License](./LICENSE-MIT)

at your option.
