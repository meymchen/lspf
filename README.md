# lspf

[![crates.io](https://img.shields.io/crates/v/lspf.svg)](https://crates.io/crates/lspf)
[![docs.rs](https://docs.rs/lspf/badge.svg)](https://docs.rs/lspf)
[![License: MIT OR Apache-2.0](https://img.shields.io/crates/l/lspf)](#license)

A Rust framework for building extensible LSP (Language Server Protocol) language servers.

`lspf` is **async-only** and designed so a developer can stand up a working
language server in very little code. You register typed handlers on a
`Server`, hand it to a transport, and the framework owns the protocol:
lifecycle, document synchronization, cancellation, bounded concurrency,
`tracing` spans, and typed server-to-client traffic through `ClientHandle`.

> **Status:** early-stage. The current published release is **0.5.2**. It
> includes the stable LSP 3.17 feature catalog, typed Commands, the
> multi-root `Workspace`, outgoing `ClientHandle` helpers, and stdio, TCP, WebSocket,
> and WASM worker-channel transports. See the
> [package changelog](./crates/lspf/CHANGELOG.md) for release history.

## Quick start

```rust,no_run
use std::sync::Arc;

use lspf::types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Hover, HoverContents, HoverParams, MarkedString,
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
        contents: HoverContents::Scalar(MarkedString::String(format!(
            "{} words",
            document.text().split_whitespace().count()
        ))),
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
    Ok(Some(CompletionResponse::Array(vec![CompletionItem {
        label: "hello".into(),
        kind: Some(CompletionItemKind::TEXT),
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
the capabilities come from the registrations themselves. A runnable copy of
the complete journey — hover, completion plus resolve, Commands, document
synchronization, multi-root workspace state, and unopened-file lookup — lives
at [`crates/lspf-hello/src/main.rs`](./crates/lspf-hello/src/main.rs), the
installable template server described under [Editor setup](#editor-setup),
with an end-to-end stdio test beside it. The
[features, capabilities, and the workspace](./docs/guides/features-and-workspace.md)
guide walks through each piece. To choose a Transport adapter, enable only
its Cargo dependencies, or implement another message-framed channel, see
[Choosing and implementing a Transport](./docs/guides/transports.md).
To embed the Client endpoint or own a stdio language-server child, follow the
[Client adoption guide](./docs/guides/client-adoption.md).
Runnable servers for individual LSP features are indexed in
[`crates/lspf/examples/README.md`](./crates/lspf/examples/README.md).

## Install

```toml
[dependencies]
lspf = "0.5.2"
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
- **Protocol details handled for you.** Lifecycle ordering, JSON-RPC
  framing, text synchronization, and UTF-8/UTF-16 position negotiation
  are built in.
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
| `Workspace`         | The cloneable handle to the connection's workspace state: folders, configuration, and documents.         |
| `FileProvider`      | The configurable resolver for resources that are not open in the editor.                                 |
| `ServerContext`     | The cheap-to-clone framework-state handle every handler receives: documents, workspace, client, scope.   |
| `ClientHandle`      | The typed handle for server-to-client notifications and requests (`ctx.client()`).                       |
| `Client`            | Configures one outbound LSP connection over a caller-provided Transport or supervised stdio child.       |
| `ClientConnection`  | Owns one initialized generic Client connection and its inbound protocol driver.                          |
| `ClientContext`     | The protocol-only context passed to reverse handlers; editor state remains caller-owned.                 |
| `ServerHandle`      | The cloneable handle for typed client-to-server calls and Client lifecycle transitions.                  |
| `ChildConnection`   | Owns one initialized stdio Client connection and its supervised language-server process.                 |
| `CancellationToken` | The cancellation signal passed to request handlers.                                                      |
| `Transport`         | A message-framed channel split into reader and writer halves for the protocol engine.                    |
| `Outcome`           | How one connection ended, returned by serving; it carries the LSP exit code but never exits the process. |

## Architecture

The full design lives next to the code:

- [`CONTEXT.md`](./CONTEXT.md) — domain language and shared vocabulary.
- [`docs/adr/`](./docs/adr/) — architecture decision records covering
  the async-only runtime, the typed Router and capability catalog, the
  protocol engine and outbound request broker, the cancellation model, the
  transport shape, the `Layer`/`Service` stack, position encoding, and more.
  ADRs describe architectural direction as well as shipped behavior; an
  accepted ADR does not by itself mean the feature has been implemented.
- [`docs/guides/features-and-workspace.md`](./docs/guides/features-and-workspace.md)
  — how to register features, where capabilities come from, who owns the
  workspace and documents, how Commands dispatch, and how `FileProvider`
  configuration works. Every example in it compiles as a doctest.
- [`docs/guides/outgoing-client.md`](./docs/guides/outgoing-client.md)
  — the server-to-client helper surface: notifications, window and workspace
  requests, dynamic registration, workspace refreshes, and work-done
  progress, with a full helper reference. Every example compiles as a
  doctest.
- [`docs/guides/client-adoption.md`](./docs/guides/client-adoption.md) — how
  to connect the Client over a custom Transport or supervised stdio child,
  register reverse handlers, set deadlines, handle failures, and shut down
  cleanly. Both walkthroughs compile as doctests.
- [`docs/guides/transports.md`](./docs/guides/transports.md) — Transport
  selection and target/feature matrices, buildable native and WASM examples,
  and the custom Transport contract.
- [`docs/guides/testing.md`](./docs/guides/testing.md) — in-memory peers,
  ordered wire capture, deterministic deadlines, and reusable Server and
  Client lifecycle journeys for external tests, plus the repository's
  deterministic protocol-session concurrency model.
- [`SECURITY.md`](./SECURITY.md) — supported Rust versions, hosts, targets,
  Cargo feature combinations, release compatibility, deprecation, and private
  vulnerability reporting.

## Current scope

Available today:

- `stdio`, single-client TCP, WebSocket, and WASM worker-channel adapters, plus
  the public custom-transport interface.
- A typed Client endpoint over custom Transport and supervised stdio child,
  with reverse handlers, bounded resources, deadlines, and lifecycle control.
- The built `Server`: typed requests, notifications, commands, the sealed
  feature catalog covering the stable LSP 3.17 features, user `Layer`s, and
  the one `configure_initialize` transaction.
- Lifecycle hooks through shutdown and exit, incremental or full text-document
  synchronization, and post-mutation document hooks.
- The multi-root `Workspace`, latest configuration settings, and
  `FileProvider`-backed unopened-file lookup.
- Typed server-to-client notifications and correlated requests through
  `ClientHandle`.
- Concurrent dispatch, bounded concurrency, request cancellation, and
  `tracing` spans.
- Rope-backed documents with UTF-8/UTF-16 position negotiation.

## Examples

Transport-specific examples reuse one shared handler module, demonstrating
that business logic does not fork between native and WASM hosts. See the
[Transport guide](./docs/guides/transports.md#buildable-examples-and-shared-handlers)
for native TCP, native WebSocket, and browser/Node worker-channel build
commands.

Run the template server straight from the workspace, or point any
LSP-aware tool at the spawned process:

```bash
cargo run -p lspf-hello
```

It is the complete typed journey — hover, completion plus resolve, Commands,
document synchronization, multi-root workspace state, and unopened-file
lookup — verified end to end by
[`crates/lspf-hello/tests/e2e.rs`](./crates/lspf-hello/tests/e2e.rs).
To wire it into a real editor instead, see [Editor setup](#editor-setup).

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

This repository is a Cargo workspace with three members:

- [`crates/lspf`](./crates/lspf) — the framework library you depend on
  (`lspf = "0.5.2"`).
- [`crates/lspf-hello`](./crates/lspf-hello) — an installable **template
  server**. It builds a `lspf-hello` binary that speaks LSP over stdio: it
  answers hover and completion (with resolve), dispatches four Commands for
  workspace roots, file reads, outgoing client helpers, and cancellable
  progress, and reads unopened files through an `OsFileProvider`. On every
  `textDocument/didOpen`, it publishes an informational diagnostic
  ("lspf saw this document open"). Fork it as the starting point for your
  own language server.
- [`crates/lspf-markdown`](./crates/lspf-markdown) — the installable
  **reference server**. It builds the `lspf-markdown` stdio binary and applies
  the framework's public document, workspace, feature, client, and testing
  interfaces to real Markdown link behavior.

### Install the server

```bash
cargo install --path crates/lspf-hello
```

This installs the `lspf-hello` binary into Cargo's bin directory
(`~/.cargo/bin` by default). Make sure that directory is on your `PATH` so
your editor can launch the server by name.

### VS Code

VS Code has no built-in generic LSP client, so install a thin generic-client
extension such as [Generic LSP Client
(v2)](https://marketplace.visualstudio.com/items?itemName=zsol.vscode-glspc),
then add this to your `settings.json`:

```json
{
  "glspc.server.command": "lspf-hello",
  "glspc.server.commandArguments": [],
  "glspc.server.languageId": ["plaintext"]
}
```

Open any plain-text (`.txt`) file and you should see the
"lspf saw this document open" diagnostic on line 1.

> During framework development you can skip the install and use the bundled
> [`tools/vscode-test-client`](./tools/vscode-test-client) instead, which
> launches the freshly built binary from `target/`.

#### Repository development

Open the repository root in VS Code and install the recommended rust-analyzer
and CodeLLDB extensions. The checked-in `.vscode` configuration provides:

- `Debug LSP client (Extension Host)`, the default end-to-end path. It builds
  `lspf-hello`, installs missing test-client dependencies from the lock file,
  compiles the client, and opens an Extension Development Host. Open a `.txt`
  file there to exercise the server.
- `Run LSP example client (select example)`, which builds the stdio examples,
  asks which one to run, and opens an Extension Development Host backed by that
  example's real process.
- `Attach to running LSP server/example`, which attaches CodeLLDB to the
  process started by either client configuration.
- build, quick-test, full workspace test, and example run tasks. The quick test
  is `cargo test -p lspf-hello`; the full task matches the main CI test command.

To debug an example, first run `Run LSP example client (select example)` and
choose an example such as `hover`. Open a `.txt` file in the new Extension
Development Host. Back in the repository window, run
`Attach to running LSP server/example`, select the process named after the
example, and set breakpoints in `crates/lspf/examples/<name>.rs`. Editor actions
now travel through the real stdio connection and stop in the Rust handler.

The Extension Host debug configuration defaults to `RUST_LOG=lspf=trace` and
`LSPF_LOG_FORMAT=json` unless the environment already has a value. Each stderr
line is one JSON event with its event fields and current span. Set
`LSPF_LOG_FORMAT=text` before launching VS Code to use compact plain text.
Run and test tasks leave both variables unchanged.

At `lspf=trace`, the framework emits five stable event shapes. Field names do
not change between inbound and outbound traffic:

| `message` | Fields |
| --- | --- |
| `rpc message` | `connection_id`, `direction`, `kind`, and, when present, `method` and `request_id` |
| `resource budget changed` | `connection_id`, `resource`, `resource_action`, `resource_current`; bounded resources also include `resource_limit`, byte budgets include `resource_bytes` and `resource_bytes_limit`, and `pending_requests` includes `direction`, `kind`, `method`, `request_id`, and optional `deadline_ms` |
| `deadline changed` | `connection_id`, `direction`, `kind`, `method`, `request_id`, `deadline`, `deadline_action`, `deadline_ms`, `deadline_elapsed_ms` |
| `request completed` | `connection_id`, `direction`, `kind`, `method`, `request_id`, `latency_ms`, `completion` |
| `connection closed` | `connection_id`, `close_cause` |

`direction` is `inbound` or `outbound`. Resource names are
`inbound_requests`, `outbound_queue`, `documents`, and `pending_requests`.
Resource actions are `admit`, `release`, `update`, `reject`, and `rollback`.
Deadline names are `handler` and `outbound_request`; deadline actions are
`armed`, `completed`, `cancelled`, and `expired`. Completion values are
`success`, `error`, `cancelled`, `deadline_expired`, `rejected`, and
`connection_closed`; close causes are `exit`, `reader_eof`, `reader_failed`,
`writer_failed`, and `initialize_failed`. Optional fields are absent rather
than set to a sentinel value.

Request and notification spans carry the same `connection_id`, `direction`,
`kind`, `method`, and optional `request_id` fields. Request spans also retain
the older debug-formatted `id` field for compatibility. Events emitted by a
handler therefore inherit the connection and call identity through their
current span.

The default events never record request parameters, response results,
Document text, or a serialized wire envelope. Applications may add their own
events inside handlers, but should treat those payloads as sensitive too.

For metrics or alerting that should not depend on tracing output, register one
connection error hook on the server:

```rust
struct State;

let _server = lspf::Server::builder(State)
    .on_error(|failure| {
        eprintln!(
            "connection {}: {:?}",
            failure.context.connection_id,
            failure.category,
        );
    })
    .build()
    .expect("server configuration is valid");
```

`ConnectionFailureCategory` distinguishes framing, protocol, Transport,
panic-isolation, overload, and close failures. The context contains the
connection ID and, when known, direction, method, and request ID. It never
contains parameters, results, document text, wire data, panic payloads, or
underlying error messages. Numeric request IDs retain their value;
peer-controlled string IDs are exposed only as `ConnectionRequestId::String`,
with their contents redacted. Method names are included only when they are
framework-owned, registered, or locally declared by a typed outbound request;
other peer-controlled method names are omitted. Each failure is reported at its
source. A panic in the hook is caught and logged; it cannot suppress a response
or interrupt the connection's cleanup. The hook observes connection failures
outside the user Layer chain, which still wraps user dispatch only.

After the server initializes, `vscode-languageclient` automatically registers
the four Commands advertised through `executeCommandProvider`. The extension
manifest supplies their titles under the `lspf hello` category in the Command
Palette. Middleware adds the active editor's URI for `Read Active File` and
`Run Outgoing Helper Journey`, then writes results to the `lspf-hello commands`
output channel. The outgoing journey exercises `workspace/applyEdit`, so it
inserts a comment at the start of the active document.

### Zed

Zed currently requires a language extension to register each language-server
adapter. Its `lsp.<name>.binary` setting can override the executable for an
adapter that Zed already knows, but it cannot register a new arbitrary server
such as `lspf-hello` from `settings.json` alone.

This repository does not yet ship a Zed extension. See Zed's
[language extension documentation](https://zed.dev/docs/extensions/languages)
to create a development extension that registers `lspf-hello`, or use the
VS Code test client above for the repository's supported editor smoke-test
path.

For repository development, the checked-in `.zed/tasks.json` has build, quick
test, full workspace test, and `hover` example tasks. The
`Attach to running LSP server/example` entry in `.zed/debug.json` opens Zed's
process picker and attaches CodeLLDB to a live server. Start that process from
the VS Code test client above, another LSP client, or a local Zed language
extension before attaching. These Zed files support Rust debugging; they do
not register `lspf-hello` as a Zed language server.

### Troubleshooting

- **`lspf-hello` not found / "command not found".** The binary isn't on your
  `PATH`. Confirm `which lspf-hello` resolves; if not, add `~/.cargo/bin` to
  your `PATH`, or use the absolute path in the editor config above.
- **The server doesn't start or no diagnostic appears.** Make sure you
  ran `cargo install --path crates/lspf-hello` after your latest changes,
  and that your editor client routes the opened file to this server. The
  example editor setup targets plain-text files; the server itself does not
  filter `didOpen` by language id. Run `lspf-hello` in a terminal with
  `RUST_LOG=lspf=trace LSPF_LOG_FORMAT=json lspf-hello` to confirm it starts
  and to emit newline-delimited JSON on stderr.
- **Edited the config but nothing changed.** Editors read LSP settings at
  startup — reload the window after editing `settings.json` (VS Code:
  *Developer: Reload Window*; Zed: reopen the workspace).
- **A direct run appears stuck.** `lspf-hello` and the framework examples speak
  LSP over stdio. Use one of the VS Code client configurations to start the
  process, then attach CodeLLDB from VS Code or Zed. Use the quick test task for
  an automated check.

## Contributing

Issues live on the [GitHub tracker](https://github.com/meymchen/lspf/issues).

Before opening a PR, please skim:

- [`CONTEXT.md`](./CONTEXT.md) — make sure the change respects the
  project's vocabulary.
- The relevant `docs/adr/*.md` — if the change revisits a decision,
  either justify the deviation in the PR description or write a new
  ADR.

Lint all Markdown with the repository's shared configuration (Node.js 24):

```bash
npx --yes markdownlint-cli2@0.22.1
```

Most mechanical Markdown issues can be fixed locally before reviewing the
result:

```bash
npx --yes markdownlint-cli2@0.22.1 --fix
```

To generate a local HTML coverage report, run:

```bash
cargo install cargo-llvm-cov --version 0.6.21 --locked
cargo coverage
```

Then open `target/coverage/html/index.html`. CI also uploads the
report as an artifact on every pull request and `main` push.

## License

Dual-licensed under either of

- [Apache License, Version 2.0](./LICENSE-APACHE)
- [MIT License](./LICENSE-MIT)

at your option.
