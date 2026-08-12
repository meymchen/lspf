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

> **Status:** early-stage. **0.2** is the current surface — the built `Server`,
> the typed `Router`, post-mutation document hooks, and the `Client` handle —
> which the examples below use. It removes the `0.1.x` `LanguageServer` trait
> outright, with no adapter and no deprecation cycle; the
> [0.1-to-0.2 migration guide](./docs/migrations/0.1-to-0.2.md) maps one onto
> the other. Hover, completion, and commands are the standard features
> implemented so far; the rest of the capability catalog and the first-party
> TCP, WebSocket, and WASM worker transports are planned but not available yet.

## Quick start

```rust,no_run
use std::sync::Arc;

use lspf::types::notification::{DidOpenTextDocument, PublishDiagnostics};
use lspf::types::{
    Diagnostic, DiagnosticSeverity, DidOpenTextDocumentParams, Position,
    PublishDiagnosticsParams, Range,
};
use lspf::{Context, Server};

/// Only your own application state — the framework owns the documents, the
/// workspace, and the client, and hands them to handlers through `Context`.
struct State;

/// A post-mutation hook for the built-in `textDocument/didOpen`: the framework
/// has already opened the document, so `ctx.documents()` sees it here.
async fn on_did_open(_state: Arc<State>, ctx: Context, params: DidOpenTextDocumentParams) {
    let uri = params.text_document.uri;
    let Some(document) = ctx.documents().get(&uri) else {
        return;
    };
    let result = ctx.client().notify::<PublishDiagnostics>(PublishDiagnosticsParams {
        uri,
        version: document.version(),
        diagnostics: vec![Diagnostic {
            range: Range {
                start: Position { line: 0, character: 0 },
                end:   Position { line: 0, character: 0 },
            },
            severity: Some(DiagnosticSeverity::INFORMATION),
            source: Some("lspf-hello".into()),
            message: "lspf saw this document open".into(),
            ..Diagnostic::default()
        }],
    });
    if let Err(error) = result {
        tracing::warn!(%error, "publishing the open diagnostic failed");
    }
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    // Logs go to stderr: stdout carries the LSP wire protocol and nothing else.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let server = Server::builder(State)
        .notification::<DidOpenTextDocument, _, _>(on_did_open)
        .build()
        .expect("the static registrations are valid");
    // Serving reports how the connection ended; the binary decides what that
    // means for the process.
    let outcome = lspf::stdio(server).serve().await?;
    std::process::exit(outcome.code());
}
```

A runnable copy lives at
[`crates/lspf-hello/src/main.rs`](./crates/lspf-hello/src/main.rs) — the
installable template server described under [Editor setup](#editor-setup).

## Install

```toml
[dependencies]
lspf = "0.2"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

`0.1.x` is the older `LanguageServer` trait API, which 0.2 removes; the
[migration guide](./docs/migrations/0.1-to-0.2.md) maps one onto the other.

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
- **Capabilities that cannot drift.** `ServerCapabilities` are generated from
  the same registrations that dispatch, so what the server advertises is what
  it serves.
- **Safe concurrent dispatch.** Requests and notifications run with a
  configurable concurrency limit (64 by default); `$/cancelRequest`
  propagates through a `CancellationToken`.
- **Protocol details handled for you.** Lifecycle ordering, JSON-RPC
  framing, text synchronization, and UTF-8/UTF-16 position negotiation
  are built in.
- **Transport escape hatch.** `stdio` is provided; implement the public
  `Transport` traits to embed lspf in tests or another message channel.

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
| `Context`           | The cheap-to-clone framework-state handle every handler receives: documents, workspace, client, scope.   |
| `Client`            | The typed handle for server-to-client notifications and requests (`ctx.client()`).                       |
| `CancellationToken` | The cancellation signal passed to request handlers.                                                      |
| `Transport`         | A message-framed channel split into reader and writer halves for the protocol engine.                    |
| `Outcome`           | How one connection ended, returned by serving; it carries the LSP exit code but never exits the process. |

## Architecture

The full design lives next to the code:

- [`CONTEXT.md`](./CONTEXT.md) — domain language and shared vocabulary.
- [`docs/adr/`](./docs/adr/) — 20 architecture decision records covering
  the async-only runtime, the typed Router and capability catalog, the
  protocol engine and outbound request broker, the cancellation model, the
  transport shape, the `Layer`/`Service` stack, position encoding, and more.
  ADRs describe architectural direction as well as shipped behavior; an
  accepted ADR does not by itself mean the feature has been implemented.

## Roadmap

Available today:

- `stdio` plus the public custom-transport interface.
- The built `Server`: typed requests, notifications, commands, hover and
  completion, user `Layer`s, and the one `configure_initialize` transaction.
- Lifecycle and incremental text-document synchronization, with
  post-mutation document hooks.
- Typed server-to-client notifications and correlated requests through
  `Client`.
- Concurrent dispatch, bounded concurrency, request cancellation, and
  `tracing` spans.
- Rope-backed documents with UTF-8/UTF-16 position negotiation.

Planned, without a committed release number:

- The remaining standard features in the capability catalog.
- The remaining lifecycle hooks and outgoing helpers.
- First-party TCP, WebSocket, and WASM worker transports.

## Examples

Run the template server straight from the workspace, or point any
LSP-aware tool at the spawned process:

```bash
cargo run -p lspf-hello
```

To wire it into a real editor instead, see [Editor setup](#editor-setup).
More examples land as the framework grows.

## Editor setup

This repository is a Cargo workspace with two members:

- [`crates/lspf`](./crates/lspf) — the framework library you depend on
  (`lspf = "0.2"`).
- [`crates/lspf-hello`](./crates/lspf-hello) — an installable **template
  server**. It builds a `lspf-hello` binary that speaks LSP over stdio and,
  on every `textDocument/didOpen`, publishes an informational diagnostic
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
