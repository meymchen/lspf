# lspf VS Code test client

[English](./README.md) | [简体中文](./README.zh-CN.md)

Minimal VSCode extension that spawns `target/debug/lspf-hello` as a
language server. Used for manual smoke testing during development —
the CI side of the same path is the `lspf-hello` end-to-end suite,
run with `cargo test -p lspf-hello` (or `cargo test --workspace`).

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
Development Host window opens. The pre-launch task builds `lspf-hello` and
starts the TypeScript compiler in watch mode. If test-client dependencies are
missing, it installs the locked versions first. A separate setup or Cargo build
is not needed for F5. Create or open any `.txt` file. You
should see the `lspf-hello` output channel come alive with LSP traffic,
and the server's `tracing` spans on its stderr (visible in the
Extension Host's debug console).

To run one of the framework examples instead, select
`Run LSP example client (select example)`. The pre-launch task builds every
stdio example, and the picker chooses the binary placed under
`target/debug/examples/`. Open a `.txt` file in the Extension Development Host
to send it real editor requests.

For Rust breakpoints, leave that Extension Development Host running. In the
repository window, start `Attach to running LSP server/example` and choose the
process whose name matches the selected example. CodeLLDB then debugs the same
process that owns the active stdio connection.

`RUST_LOG=lspf=trace` and `LSPF_LOG_FORMAT=json` are set by default. The
`lspf-hello` output channel receives one JSON event per line. Export either
variable before launching VSCode to override it; use `LSPF_LOG_FORMAT=text`
for compact plain text.

## Commands

Once the language client has initialized, open the Command Palette and choose
an entry in the `lspf hello` category:

- `Show Workspace Roots`
- `Read Active File`
- `Run Outgoing Helper Journey`
- `Run Cancellable Progress`

`vscode-languageclient` automatically registers the commands advertised by the
server's `executeCommandProvider`; `package.json` only gives those commands
titles in the Command Palette. Its `executeCommand` middleware adds the active
editor's URI when a command needs one. Results are written to the
`lspf-hello commands` output channel. The outgoing journey inserts a comment
at the start of the active document while exercising `workspace/applyEdit`.

## What this validates

The wire-level claims a real editor makes on the server: VSCode's own
`initialize` payload deserializes into `lsp_types::InitializeParams`, the
generated `ServerCapabilities` advertise incremental document sync, and the
reply, the `didOpen` that follows it, and the diagnostic the server publishes
all round-trip through stdio framing.
