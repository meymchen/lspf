# lspf VSCode test client

Minimal VSCode extension that spawns `target/debug/examples/hello` as a
language server. Used for manual smoke testing during development —
the CI side of the same path is `cargo test --test smoke`.

## Setup (one-time)

```sh
cd tools/vscode-test-client
npm install
npm run compile
```

## Build the server

From the repo root:

```sh
cargo build --example hello
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

Commit 1's wire-level claim: VSCode's real `initialize` payload
deserializes into `lsp_types::InitializeParams` and our reply round-trips
back through stdio framing. Anything further (diagnostics, document
sync) is commit 2+.
