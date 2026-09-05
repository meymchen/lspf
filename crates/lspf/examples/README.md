# Feature example servers

Each file is a runnable stdio language server focused on a small set of LSP
methods. The parsers and languages are intentionally small so that the protocol
interaction stays visible.

| Example | Demonstrated methods |
| --- | --- |
| `code_actions` | `textDocument/codeAction` |
| `code_lens` | `textDocument/codeLens`, `codeLens/resolve`, command and workspace edit |
| `colors` | `textDocument/documentColor`, `textDocument/colorPresentation` |
| `formatting` | document, range, and on-type formatting |
| `goto` | declaration, definition, implementation, type definition, and references |
| `hover` | `textDocument/hover` |
| `inlay_hints` | `textDocument/inlayHint`, `inlayHint/resolve` |
| `links` | `textDocument/documentLink`, `documentLink/resolve` |
| `notebooks` | Notebook open, change, save, and close hooks; hover over synchronized cell text |
| `publish_diagnostics` | push diagnostics from document open and change hooks |
| `pull_diagnostics` | document and workspace diagnostic requests |
| `rename` | `textDocument/prepareRename`, `textDocument/rename` |
| `semantic_tokens` | full, full-delta, and range semantic tokens |
| `symbols` | document and workspace symbol requests |
| `server_features` | commands, progress, configuration, async work, and dynamic client registration |
| `blocking_work` | completion while blocking work runs on a dedicated thread pool |

Run a server over stdio with Cargo:

```console
cargo run -p lspf --example hover
```

That direct command waits for an LSP client on stdin. For an interactive VS
Code session, open the repository root and run
`Run LSP example client (select example)`. After the Extension Development Host
opens, run `Attach to running LSP server/example` in the repository window and
pick the example process. Zed's `.zed/debug.json` provides the same attach
entry, so Zed can debug a process already started by an LSP client.

Every example logs to stderr, because stdout carries the LSP wire protocol.
`RUST_LOG` selects the events — `RUST_LOG=lspf=trace` for the full framework
trace — and `LSPF_LOG_FORMAT` selects their shape: `json` writes one
machine-readable event per line, anything else writes plain text. Both are
handled by [`example_logging/mod.rs`](./example_logging/mod.rs), which every
example installs, the transport ones included.

The `blocking_work` example uses `tokio::task::spawn_blocking` because lspf
handlers are async-only. The `server_features` example installs its completion
route before initialization, then uses `client/registerCapability` and
`client/unregisterCapability` to control whether the client sends requests to
that route. Local routes cannot be added or removed after initialization because
the server router is frozen.

## Notebook example

`notebooks` advertises synchronization for `jupyter-notebook` notebooks,
including save notifications. Its lifecycle hooks log the synchronized Notebook
version and cell count through `window/logMessage`. Hover over a cell shows its
Notebook and Document versions and its current text. Cell text comes from the
same Documents view used by text-document handlers.

In VS Code, select `Run Notebook example client` to build the server and open
[`notebooks/demo.ipynb`](./notebooks/demo.ipynb) in an Extension Development Host.
The general example picker also includes `notebooks`. Hover over a code cell,
edit or rearrange cells, and save; the `lspf notebooks` output channel shows the
lifecycle hooks. No kernel is needed. Attach to the `notebooks` process with
`Attach to running LSP server/example` for Rust breakpoints.

Other Notebook-aware LSP clients can launch it with
`cargo run -p lspf --example notebooks`. To test without an editor, run:

```console
cargo test -p lspf --example notebooks --features testing --locked
```

The tests drive the example's real Server through `MemoryTransport`: capability
advertisement, all four lifecycle hooks, UTF-16 incremental cell edits,
fragment-distinct cell URIs, structural replacement, and rollback after a
rejected batch. They run in the existing CI jobs that enable `testing` and use
`--all-targets`.

## Transport examples

The transport examples reuse the handlers in `shared/mod.rs`. Build each native
adapter with only its required feature:

```console
cargo check -p lspf --example native_tcp --no-default-features --features tcp
cargo check -p lspf --example native_websocket --no-default-features --features websocket
```

To drive one from a real editor, open the repository in VS Code and run
`Run LSP example client over a socket (select transport)`. The client starts the
chosen example and connects to its port, so VS Code's own language client
exercises the adapter. See the
[test client README](../../../tools/vscode-test-client/README.md).

Zed launches every language server as a command over stdio and offers no socket
option, so its lspf journeys stay on the stdio examples above.

For an unattended check of both adapters, the
[transport probe](../../../tools/lsp-transport-probe/README.md) builds, serves,
and asserts one full session per transport:

```console
node tools/lsp-transport-probe/main.mjs both
```

`shared_server` serves the same handlers over stdio on native targets and also
checks the runtime-only WASM path. `worker_channel` exports a server for a
browser or Node Worker. The [Transport guide](https://lspf.dev/guides/transports)
contains the WASM build and host commands.
