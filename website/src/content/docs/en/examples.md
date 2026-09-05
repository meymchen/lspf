---
title: Feature example servers
description: Run small language servers that each demonstrate one part of LSP.
---

Each example is a runnable stdio language server. The parsers are deliberately small,
so the protocol interaction stays visible.

## Run an example

```console
cargo run -p lspf --example hover
```

The process waits for an LSP client on stdin. In this repository, run
`Run LSP example client (select example)` from VS Code to open an Extension Development
Host connected to the selected server.

Every example logs to stderr because stdout carries the LSP wire protocol.
`RUST_LOG` selects the events—use `RUST_LOG=lspf=trace` for the full framework
trace—and `LSPF_LOG_FORMAT=json` writes one machine-readable event per line.
Any other format value produces plain text.

## Language features

| Example | What it demonstrates |
| --- | --- |
| [`code_actions`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/code_actions.rs) | Code actions |
| [`code_lens`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/code_lens.rs) | Code lens, resolve, commands, and workspace edits |
| [`colors`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/colors.rs) | Document colors and color presentations |
| [`formatting`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/formatting.rs) | Document, range, and on-type formatting |
| [`goto`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/goto.rs) | Declaration, definition, implementation, type definition, and references |
| [`hover`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/hover.rs) | Hover results built from synchronized document text |
| [`inlay_hints`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/inlay_hints.rs) | Inlay hints and resolve |
| [`links`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/links.rs) | Document links and resolve |
| [`publish_diagnostics`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/publish_diagnostics.rs) | Push diagnostics from document hooks |
| [`pull_diagnostics`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/pull_diagnostics.rs) | Document and workspace diagnostic requests |
| [`rename`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/rename.rs) | Prepare rename and rename |
| [`semantic_tokens`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/semantic_tokens.rs) | Full, delta, and range semantic tokens |
| [`symbols`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/symbols.rs) | Document and workspace symbols |

## Framework behavior

| Example | What it demonstrates |
| --- | --- |
| [`server_features`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/server_features.rs) | Commands, progress, configuration, async work, and dynamic registration |
| [`blocking_work`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/blocking_work.rs) | Keeping blocking work off async executor threads |

## Transport examples

The transport examples reuse the same handlers so application logic does not change
with the host:

```console
cargo check -p lspf --example native_tcp --no-default-features --features tcp
cargo check -p lspf --example native_websocket --no-default-features --features websocket
```

Run `Run LSP example client over a socket (select transport)` in VS Code and
choose `tcp` or `websocket` to start an example and connect the editor's own
language client to it. Zed launches language servers over stdio and cannot
connect to these socket examples.

For an unattended check of both adapters, run:

```console
node tools/lsp-transport-probe/main.mjs both
```

The [transport guide](guides/transports) covers native sockets and browser or Node
workers, including build and host commands.
