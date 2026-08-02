# lspf VSCode test client

Minimal VSCode extension that spawns `target/debug/lspf-hello` as a
language server. Used for manual smoke testing during development —
the CI side of the same path is the `lspf-hello` end-to-end suite,
run with `cargo test -p lspf-hello` (or `cargo test --workspace`).

## Setup (one-time)

```sh
cd tools/vscode-test-client
npm install
npm run compile
```

The extension's server-path resolution has a small unit test; run it with:

```sh
npm test
```

## Build the server

From the repo root:

```sh
cargo build -p lspf-hello
```

## Launch

Open `tools/vscode-test-client/` in VSCode and press F5. An Extension
Development Host window opens — create or open any `.txt` file. You
should see the `lspf-hello` output channel come alive with LSP traffic,
and the server's `tracing` spans on its stderr (visible in the
Extension Host's debug console).

`RUST_LOG=lspf=trace` is set by default; override by exporting
`RUST_LOG` before launching VSCode.

## What this validates

The wire-level claims a real editor makes on the server: VSCode's own
`initialize` payload deserializes into `lsp_types::InitializeParams`, the
generated `ServerCapabilities` advertise incremental document sync, and the
reply, the `didOpen` that follows it, and the diagnostic the server publishes
all round-trip through stdio framing.
