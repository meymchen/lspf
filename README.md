# lspf

[![crates.io](https://img.shields.io/crates/v/lspf.svg)](https://crates.io/crates/lspf)
[![docs.rs](https://docs.rs/lspf/badge.svg)](https://docs.rs/lspf)
[![License: MIT OR Apache-2.0](https://img.shields.io/crates/l/lspf)](#license)

[English](./README.md) | [简体中文](./README.zh-CN.md)

A Rust framework for building extensible LSP (Language Server Protocol) language servers.

`lspf` is **async-only** and designed so a developer can stand up a working
language server in very little code. You register typed handlers on a
`Server`, hand it to a transport, and the framework owns the protocol:
lifecycle, document synchronization, cancellation, bounded concurrency,
`tracing` spans, and typed server-to-client traffic through `Client`.

> **Status:** early-stage. **0.3** is the current surface of this repository
> — the sealed feature catalog covering the stable LSP 3.17 features, Commands,
> the multi-root `Workspace`, `FileProvider`-backed unopened-file lookup, and
> configurable document synchronization — which the examples below use. It is
> not published yet: crates.io still carries **0.2**, and the
> [changelog](./CHANGELOG.md) records what 0.3 adds on top of it. Hover,
> completion, and commands were the standard features implemented in 0.2; the
> first-party TCP, WebSocket, and WASM worker-channel transports are
> implemented.

## Quick start

```rust,no_run
use std::sync::Arc;

use lspf::types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Hover, HoverContents, HoverParams, MarkedString,
};
use lspf::{CancellationToken, Context, LspError, OsFileProvider, Server};

/// Only your own application state — the framework owns the documents, the
/// workspace, and the client, and hands them to handlers through `Context`.
struct State;

/// A standard typed feature. The `features::hover()` descriptor fixes the
/// wire method, this handler's parameter and result types, and the
/// `hoverProvider` capability the server will advertise — all at once.
async fn hover(
    _state: Arc<State>,
    ctx: Context,
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
    _ctx: Context,
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
    ctx: Context,
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
Runnable servers for individual LSP features are indexed in
[`crates/lspf/examples/README.md`](./crates/lspf/examples/README.md).

## Install

```toml
[dependencies]
lspf = "0.3"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

`0.3` is the latest published release, and the quickstart above targets it.

`lspf`'s own `Cargo.toml` already pulls in `lsp-types`, `tokio`, `tracing`,
`serde`, and the rest of the runtime stack, so you only need to opt in to the
`tokio` features you actually use.

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
  the protocol and read through `Context`; unopened files resolve through a
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
| `Context`           | The cheap-to-clone framework-state handle every handler receives: documents, workspace, client, scope.   |
| `Client`            | The typed handle for server-to-client notifications and requests (`ctx.client()`).                       |
| `CancellationToken` | The cancellation signal passed to request handlers.                                                      |
| `Transport`         | A message-framed channel split into reader and writer halves for the protocol engine.                    |
| `Outcome`           | How one connection ended, returned by serving; it carries the LSP exit code but never exits the process. |

## Architecture

The full design lives next to the code:

- [`CONTEXT.md`](./CONTEXT.md) — domain language and shared vocabulary.
- [`docs/adr/`](./docs/adr/) — 24 architecture decision records covering
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
- [`docs/guides/migrating-to-0.4.md`](./docs/guides/migrating-to-0.4.md)
  — the 0.3 → 0.4 breaking changes and how to update.
- [`docs/guides/transports.md`](./docs/guides/transports.md) — the 0.5
  Transport selection and target/feature matrices, buildable native and WASM
  examples, custom Transport contract, and explicit deployment non-goals.

## Roadmap

Available today:

- `stdio`, single-client TCP, WebSocket, and WASM worker-channel adapters, plus
  the public custom-transport interface.
- The built `Server`: typed requests, notifications, commands, the sealed
  feature catalog covering the stable LSP 3.17 features, user `Layer`s, and
  the one `configure_initialize` transaction.
- Lifecycle, incremental or full text-document synchronization, and the
  post-mutation document hooks.
- The multi-root `Workspace`, latest configuration settings, and
  `FileProvider`-backed unopened-file lookup.
- Typed server-to-client notifications and correlated requests through
  `Client`.
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

## Editor setup

This repository is a Cargo workspace with two members:

- [`crates/lspf`](./crates/lspf) — the framework library you depend on
  (`lspf = "0.2"`).
- [`crates/lspf-hello`](./crates/lspf-hello) — an installable **template
  server**. It builds a `lspf-hello` binary that speaks LSP over stdio: it
  answers hover and completion (with resolve), dispatches the
  `lspf-hello.workspaceRoots` and `lspf-hello.readFile` Commands, reads
  unopened files through an `OsFileProvider`, and — on every
  `textDocument/didOpen` — publishes an informational diagnostic
  ("lspf saw this document open"). Fork it as the starting point for your
  own language server.

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

### Troubleshooting

- **`lspf-hello` not found / "command not found".** The binary isn't on your
  `PATH`. Confirm `which lspf-hello` resolves; if not, add `~/.cargo/bin` to
  your `PATH`, or use the absolute path in the editor config above.
- **The server doesn't start or no diagnostic appears.** Make sure you
  ran `cargo install --path crates/lspf-hello` after your latest changes,
  and that your editor client routes the opened file to this server. The
  example editor setup targets plain-text files; the server itself does not
  filter `didOpen` by language id. Run `lspf-hello` in a terminal with
  `RUST_LOG=lspf=trace` to confirm it starts and to see LSP traffic on stderr.
- **Edited the config but nothing changed.** Editors read LSP settings at
  startup — reload the window after editing `settings.json` (VS Code:
  *Developer: Reload Window*; Zed: reopen the workspace).

## Contributing

Issues live on the GitHub tracker at
[meymchen/lspf](https://github.com/meymchen/lspf/issues), managed via
`gh`. Triage uses a fixed label set — `needs-triage`, `needs-info`,
`ready-for-agent`, `ready-for-human`, `wontfix` — so an agent or a
human can pick up an issue without re-classifying it.

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
