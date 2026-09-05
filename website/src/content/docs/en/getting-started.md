---
title: Getting started
description: Install lspf and run your first typed stdio language server.
---

`lspf` is an async Rust framework for implementing Language Server Protocol servers and
clients. This guide creates the smallest useful stdio server.

## Requirements

- Rust 1.98 or newer.
- Familiarity with `async fn` and Cargo.

## Install

Create a binary crate and add lspf with its default `stdio` transport:

```console
cargo new my-language-server
cd my-language-server
cargo add lspf
cargo add tokio --features macros,rt-multi-thread
```

The default lspf feature set includes stdio. Select transport features explicitly if
your application uses TCP, WebSocket, or a worker channel.

## Build a server

```rust
use std::sync::Arc;

use lspf::types::{Hover, HoverContents, HoverParams, MarkedString};
use lspf::{CancellationToken, LspError, Server, ServerContext};

struct State;

async fn hover(
    _state: Arc<State>,
    _ctx: ServerContext,
    _params: HoverParams,
    _cancellation: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    Ok(Some(Hover {
        contents: HoverContents::Scalar(MarkedString::String("Hello from lspf".into())),
        range: None,
    }))
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .feature(lspf::features::hover(), hover)
        .build()?;

    let outcome = lspf::stdio(server).serve().await?;
    std::process::exit(outcome.code());
}
```

The `hover()` descriptor fixes the wire method, parameter type, result type, and
advertised capability together. There is no separate `ServerCapabilities` value to
keep synchronized.

## Next steps

- Follow [Build a language server](tutorials/server) for a complete server with
  document synchronization, diagnostics, and a command.
- Read [Core concepts](concepts) for lspf's ownership model.
- Browse the complete, versioned API on [docs.rs](https://docs.rs/lspf).
