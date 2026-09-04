# lspf VS Code test client

Minimal VS Code extension that runs the in-tree `lspf-markdown` reference
server or one of the framework examples through VS Code's real language
client. The automated counterpart is the packaged reference-server journey,
run with `cargo test -p lspf-markdown --test packaged_editor_journey`.

## Setup

Node.js 24 is required. From the repository root, install the exact versions
from `package-lock.json`, compile the extension, and run its unit tests:

```sh
npm --prefix tools/vscode-test-client ci
npm --prefix tools/vscode-test-client run compile
npm --prefix tools/vscode-test-client test
```

## Launch

Open the repository root in VS Code and select
`Debug LSP client (Extension Host)` from the Run and Debug view. An Extension
Development Host window opens. The pre-launch task builds `lspf-markdown` and
starts the TypeScript compiler in watch mode. If test-client dependencies are
missing, it installs the locked versions first. A separate setup or Cargo build
is not needed for F5. Create or open a Markdown file. You should see the
`lspf-markdown` output channel come alive with LSP traffic.

To validate an installed reference server instead of the workspace binary,
set `LSPF_MARKDOWN_SERVER` to its absolute path before launching the same
Extension Host configuration. The complete three-editor procedure lives in
[`../../editor-validation`](../../editor-validation).

To run one of the framework examples instead, select
`Run LSP example client (select example)`. The pre-launch task builds every
stdio example, and the picker chooses the binary placed under
`target/debug/examples/`. Open a `.txt` file in the Extension Development Host
to send it real editor requests.

For Rust breakpoints, leave that Extension Development Host running. In the
repository window, start `Attach to running LSP server/example` and choose the
process whose name matches the selected example. CodeLLDB then debugs the same
process that owns the active stdio connection.

## Connecting over TCP or WebSocket

Select `Run LSP example client over a socket (select transport)` and pick `tcp`
or `websocket`. The pre-launch task builds `native_tcp` and `native_websocket`
with only their adapter feature. The client then starts the chosen example and
connects to the port it binds — `127.0.0.1:9257` for TCP, `127.0.0.1:9258` for
WebSocket — so the socket adapters are driven by VS Code's own language client
rather than by a scripted one.

Open a `.txt` file in the Extension Development Host. Hover and completion come
from [`examples/shared/mod.rs`](../../crates/lspf/examples/shared/mod.rs), which
is the same handler set every transport example serves. The `lspf native_tcp`
(or `lspf native_websocket`) output channel carries the server's `tracing`
output, exactly as the stdio channels do. Rust breakpoints work the same way as
over stdio: attach to the `native_tcp` or `native_websocket` process.

Getting there needs two things that stdio gets for free. `vscode-languageclient`
pipes a server's stderr into the output channel only when it spawned the process
itself, so with a socket transport the extension forwards that stderr and hands
the client the same channel rather than letting it open a second one. And the
two transport examples install their own `tracing` subscriber — without one,
`RUST_LOG` selects events that nothing records and the channel stays empty.

`LSPF_TEST_TRANSPORT` selects the transport (`stdio`, `tcp`, or `websocket`) and
defaults to `stdio`, so every existing launch configuration is unaffected. The
two socket examples ignore `LSPF_TEST_EXAMPLE`: only they bind a port, and the
stdio examples have no listener to dial.

Each transport example binds once, accepts one client, and drops its listener,
so one server serves exactly one Extension Development Host. Restart the launch
configuration to get a fresh server.

Zed cannot do this. It launches every language server as a command over stdio
(`zed::Command` is a binary, arguments, and environment), with no socket option,
so its lspf journeys stay on the stdio examples.

`RUST_LOG=lspf=trace` and `LSPF_LOG_FORMAT=json` are set by default, and every
framework example this client launches honours them, so its output channel
receives one JSON event per line. Export either variable before launching VS
Code to override it; `LSPF_LOG_FORMAT=text` gives the plain-text format
instead, one line of `elapsed level span_names: message fields`.

## Tests

`npm test` compiles the client and runs `test/**/*.test.ts`; `npm run
test:coverage` adds the lcov report SonarCloud reads.

The coverage run excludes `out/!(extensionCore).js`. Every test imports its
subject from `src/`, except `extension.test.ts`, which loads the compiled
`out/extensionCore.js` because `src/extensionCore.ts` uses the `.js` import
specifiers TypeScript requires and Node cannot resolve to `.ts`. That compiled
module pulls in the compiled copy of every other module, and `--enable-source-maps`
maps those copies back onto the same `src/*.ts` paths — a second, mostly unhit
record that would otherwise report tested code as uncovered.

## What this validates

The wire-level claims a real editor makes on the server: VS Code's own
`initialize` payload deserializes into `lsp_types::InitializeParams`, the
generated `ServerCapabilities` advertise incremental document sync, and the
reply, document notifications, diagnostics, hover, and definition requests all
round-trip through stdio framing. Framework examples additionally exercise the
same client over stdio, TCP, and WebSocket adapters.
