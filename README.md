# lspf

[![crates.io](https://img.shields.io/crates/v/lspf.svg)](https://crates.io/crates/lspf)
[![docs.rs](https://docs.rs/lspf/badge.svg)](https://docs.rs/lspf)
[![License: MIT OR Apache-2.0](https://img.shields.io/crates/l/lspf)](#license)

A Rust framework for building extensible LSP (Language Server Protocol) language servers.

`lspf` is **async-only** and designed so a developer can stand up a working
language server in very little code. Capabilities are auto-derived from the
`LanguageServer` trait, the default layer stack installs lifecycle, panic
catching, `$/cancelRequest` routing, bounded concurrency, and `tracing`
spans, and outgoing helpers (`publish_diagnostics`, `show_message`,
`apply_edit`, …) are exposed on the per-request `Context` every handler
receives.

> **Status:** `0.1.0-alpha.3` is the third alpha; the first non-alpha
> `0.1.0` release is still planned, gated on the `Layer`/`Service`
> generalization landing. The architecture is scoped in
> [`CONTEXT.md`](./CONTEXT.md) and [`docs/adr/`](./docs/adr/); the `stdio`
> transport, the `LanguageServer` trait, and the basic dispatcher are
> wired up. Subsequent commits add the `Layer`/`Service` generalization,
> the remaining transports (TCP, WebSocket, worker-channel for WASM), and
> the full pygls-equivalent outgoing helper coverage.

## Quick start

```rust
use lspf::types::{
    Diagnostic, DiagnosticSeverity, DidOpenTextDocumentParams, Position,
    PublishDiagnosticsParams, Range,
};
use lspf::{Context, LanguageServer};

struct Hello;

impl LanguageServer for Hello {
    async fn text_document_did_open(
        &self,
        ctx: &Context,
        params: DidOpenTextDocumentParams,
    ) {
        ctx.publish_diagnostics(PublishDiagnosticsParams {
            uri: params.text_document.uri,
            version: Some(params.text_document.version),
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
    }
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    lspf::stdio(Hello).serve().await
}
```

A runnable copy lives at
[`crates/lspf-hello/src/main.rs`](./crates/lspf-hello/src/main.rs) — the
installable template server described under [Editor setup](#editor-setup).

## Install

```toml
[dependencies]
lspf = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tracing-subscriber = "0.3"
```

`Cargo.toml` already pulls in `lsp-types`, `tokio`, `tracing`, `serde`, and
the rest of the runtime stack, so you only need to opt in to the
`tokio` features you actually use.

## Why lspf

- **Async-first.** The framework is `async fn` end to end; no `tower::Layer`
  interop, no sync escape hatch.
- **Smallest viable server.** Implement the `LanguageServer` trait, hand
  the value to `lspf::stdio(...)`, and you have a working LSP server.
- **Capabilities auto-derived.** Each LSP feature is an associated `const`
  on the trait; the framework turns the consts into the
  `ServerCapabilities` response for you ([ADR 0004](./docs/adr/0004-capability-auto-derivation.md)).
- **Composability, on our terms.** A focused `Layer` trait (narrower than
  `tower::Layer`) adds cross-cutting behavior without a third-party
  dependency on the dispatcher ([ADR 0010](./docs/adr/0010-own-layer-trait-not-tower.md)).
- **pygls-grade helpers out of the box.** The full set of pygls's
  outgoing notifications and requests ships as methods on `Context`
  ([ADR 0008](./docs/adr/0008-v1-scope-server-only-pygls-helper-coverage.md)).
- **WASM-friendly.** The `worker_channel` transport wraps a JS
  `MessagePort` for in-browser Monaco / Theia-web integration
  ([ADR 0011](./docs/adr/0011-transport-shape-and-v1-adapters.md)).

## Concepts

The vocabulary below is taken from [`CONTEXT.md`](./CONTEXT.md); the
project deliberately standardizes on these terms in the public API and
the docs.

| Term             | Meaning                                                                                         |
| ---------------- | ----------------------------------------------------------------------------------------------- |
| Handler          | An `async fn` registered to respond to an LSP method or notification.                           |
| Built-in handler | A handler the framework ships out of the box (lifecycle, text-document sync).                    |
| User handler     | A handler you register. User handlers override built-ins via registration, not subclassing.     |
| Document         | A text resource the framework tracks on your behalf (URI, language id, version, contents).      |
| Documents        | The concurrency-safe handle to every tracked document, available on every handler's `Context`.  |
| Command          | A user-registered `async` closure dispatched on `workspace/executeCommand` by name.             |
| Context          | Per-request framework-state handle (`Documents`, outgoing helpers, request id, `tracing` span). |
| Transport        | The message-framed channel over which LSP JSON-RPC envelopes flow.                              |
| Layer            | A composable wrapper around a `Service` that adds cross-cutting behavior.                      |
| Service          | The internal abstraction the dispatcher and every `Layer` implement.                            |
| Default stack    | The built-in set of `Layer`s installed by the transport builders.                               |

## Architecture

The full design lives next to the code:

- [`CONTEXT.md`](./CONTEXT.md) — domain language and shared vocabulary.
- [`docs/adr/`](./docs/adr/) — 14 architecture decision records covering
  async-only runtime, the dispatcher design, capability auto-derivation,
  the cancellation model, the transport shape, the `Layer`/`Service`
  generalization, and more.

## Roadmap

The `0.1.x` series works through the ADRs in order. The headline
milestones:

- **0.1.x** — `stdio` transport, `LanguageServer` trait, basic
  dispatcher, capability auto-derivation, `Context`-based outgoing
  helpers (`publish_diagnostics` is wired in 0.1.0; the rest of the
  pygls-equivalent set follows).
- **0.2.x** — `Layer`/`Service` generalization (ADR 0010), default
  stack: lifecycle, panic catching, `$/cancelRequest`, bounded
  concurrency (64 in-flight by default), `tracing` spans.
- **0.3.x** — `tcp` and `websocket` transports; concurrent
  spawn-based dispatch.
- **0.4.x** — `worker_channel` transport for WASM-in-browser; full
  pygls-equivalent outgoing helper coverage on `Context`.

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
  (`lspf = "0.1"`).
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

Zed launches a custom server straight from `settings.json`. Register the
binary under `lsp` and attach it to the built-in **Plain Text** language:

```json
{
  "languages": {
    "Plain Text": {
      "language_servers": ["lspf-hello"]
    }
  },
  "lsp": {
    "lspf-hello": {
      "binary": {
        "path": "lspf-hello",
        "arguments": []
      }
    }
  }
}
```

Open any `.txt` file; the "lspf saw this document open" diagnostic appears
on the first line. If Zed cannot find `lspf-hello` on your `PATH`, set
`path` to the absolute path printed by `which lspf-hello`
(`where lspf-hello` on Windows).

### Troubleshooting

- **`lspf-hello` not found / "command not found".** The binary isn't on your
  `PATH`. Confirm `which lspf-hello` resolves; if not, add `~/.cargo/bin` to
  your `PATH`, or use the absolute path in the editor config above.
- **The server doesn't start or no diagnostic appears.** Make sure you
  ran `cargo install --path crates/lspf-hello` after your latest changes,
  and that the file you opened is recognized as plain text (the server only
  reacts to `plaintext` / **Plain Text** documents). Run `lspf-hello` in a
  terminal with `RUST_LOG=lspf=trace` to confirm it starts and to see LSP
  traffic on stderr.
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

To generate a local HTML coverage report, run:

```bash
cargo coverage
```

Then open `target/coverage/html/index.html`. CI also uploads the
report as an artifact on every pull request and `main` push.

## License

Dual-licensed under either of

- [Apache License, Version 2.0](./LICENSE-APACHE)
- [MIT License](./LICENSE-MIT)

at your option.
