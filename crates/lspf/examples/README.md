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

The `blocking_work` example uses `tokio::task::spawn_blocking` because lspf
handlers are async-only. The `server_features` example installs its completion
route before initialization, then uses `client/registerCapability` and
`client/unregisterCapability` to control whether the client sends requests to
that route. Local routes cannot be added or removed after initialization because
the server router is frozen.
