---
title: Core concepts
description: Learn the ownership and dispatch model behind lspf.
---

lspf draws a firm line between application state and per-connection protocol state.
Understanding that boundary makes the rest of the API predictable.

## Server and handlers

A `Server` owns exactly one LSP connection. `Server::builder(state)` registers typed
request handlers, notification handlers, commands, lifecycle hooks, and service
layers. The built server is then served over one transport.

Handlers receive your state as `Arc<State>`, plus a cheap-to-clone `ServerContext`.
Request handlers also receive a `CancellationToken`.

## ServerContext

`ServerContext` is the path to framework-owned state for the current connection:

- `ctx.documents()` reads synchronized text documents.
- `ctx.notebooks()` reads notebook structure.
- `ctx.workspace()` reads workspace folders and configuration.
- `ctx.client()` sends typed requests and notifications to the editor.
- `ctx.partial_results()` reports chunks for supported requests.

Do not store a context or one of its views in global application state. Use the value
provided for the active call so connection ownership stays explicit.

## Features and capabilities

A feature descriptor binds an LSP method to its parameter and result types. Registering
the descriptor also contributes its capability to initialization. Conflicting
registrations are build errors instead of silent last-write-wins behavior.

```rust
Server::builder(state)
    .feature(lspf::features::hover(), hover)
    .feature(lspf::features::completion(options), completion)
    .build()?;
```

## Documents and workspace

Document synchronization is a protocol built-in. lspf applies `didOpen`, `didChange`,
and `didClose` before running your post-mutation hooks. Handlers see immutable,
rope-backed document snapshots through `DocumentsView`.

The `Workspace` keeps multi-root folders, initialization values, configuration, and
document access together. A configurable `FileProvider` resolves unopened resources.

## Concurrency and cancellation

Requests and notifications run concurrently under a finite resource policy. Incoming
`$/cancelRequest` messages signal the request's `CancellationToken`; dropping a future
also stops async work naturally. Keep blocking work separately bounded.
