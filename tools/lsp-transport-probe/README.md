# LSP transport probe

A dependency-free LSP client that drives the native TCP and WebSocket transport
examples over a real socket, without an editor in the loop.

For interactive work, prefer the real editor client: the
[VS Code test client](../vscode-test-client/README.md) connects over TCP or
WebSocket through its `Run LSP example client over a socket` launch
configuration. This probe is the unattended counterpart — it asserts a fixed
session and exits non-zero, so it can run without a GUI.

The probe builds the example with only its adapter feature, starts it, runs one
LSP session against the handlers in
[`crates/lspf/examples/shared/mod.rs`](../../crates/lspf/examples/shared/mod.rs),
and checks each response. It exits non-zero when a check fails.

```console
node tools/lsp-transport-probe/main.mjs both
node tools/lsp-transport-probe/main.mjs tcp
node tools/lsp-transport-probe/main.mjs websocket
```

It needs only Node's standard library and the global `WebSocket` added in Node
22; the repository pins Node 24.

## What each session covers

| Check | Why it is here |
| --- | --- |
| `initialize` advertises hover and completion | The capability payload survives the adapter |
| `textDocument/hover` returns `shared` | A typed feature route answers over the socket |
| `textDocument/completion` returns the `shared` item | A second route answers on the same connection |
| `shared/ping` returns `shared:<transport>` | A custom typed request round-trips its object params |
| `shutdown` succeeds with `params` omitted | The parameterless spelling clients actually send |
| `exit` with `"params": null` ends the process | The other parameterless spelling must behave alike |
| The server exits 0 | The adapter closes through the ordinary outcome path |

Both framings are exercised as the transports define them: TCP carries
`Content-Length` framed envelopes, and WebSocket carries one JSON envelope per
message with no header.

## Debugging the server side

The probe owns the server by default, which leaves nothing for a debugger to
attach to. `--attach` skips the build and spawn and connects to a server that is
already listening:

```console
cargo run -p lspf --example native_tcp --no-default-features --features tcp
node tools/lsp-transport-probe/main.mjs tcp --attach
```

Each transport example binds once, accepts one client, and drops its listener,
so every probe run needs its own server process.

## Addresses

`127.0.0.1:9257` for TCP and `127.0.0.1:9258` for WebSocket. Both are hard-coded
by the `serve` call in their example, so `main.mjs` repeats them; change them
together.
