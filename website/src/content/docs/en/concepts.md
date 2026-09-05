---
title: Core concepts
description: Learn the ownership and dispatch model behind lspf.
---

<!-- markdownlint-disable-next-line MD025 -->
# Core concepts

lspf draws a firm line between application state and per-connection protocol state.
Understanding that boundary makes the rest of the API predictable.

## Message paths and ownership

<!-- markdownlint-disable-next-line MD033 -->
<ArchitectureFlow />

The protocol engine handles lifecycle, cancellation, and synchronized state.
User Layers wrap user dispatch; they cannot intercept protocol-owned mutations.
For document notifications, the engine applies the change first, then dispatches
any registered post-mutation hook through the Service stack.

The fixed stack runs from panic isolation to tracing, bounded concurrency, user
Layers, and finally the Router service. The last registered user Layer is the
outermost user Layer. The Router freezes during initialization, after static and
initialize-conditional registrations have been combined.

The connection's private protocol session owns request correlation, resource
admission, deadlines, the outbound queue, and task cleanup. `ServerContext` exposes
read views and typed handles; your `Arc<State>` holds application-owned data.

These paths correspond to [`ProtocolEngine::dispatch`](https://github.com/meymchen/lspf/blob/main/crates/lspf/src/engine.rs),
[`build_service_stack`](https://github.com/meymchen/lspf/blob/main/crates/lspf/src/service.rs),
and the private [`ProtocolSession`](https://github.com/meymchen/lspf/blob/main/crates/lspf/src/session.rs).

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

User request and notification handlers run concurrently under a finite resource
policy. Protocol-owned document mutations run in the read loop before their hooks.
Incoming `$/cancelRequest` messages signal the request's `CancellationToken`.
Cancellation can stop async work, but does not roll back side effects or stop
already-started blocking tasks. Keep blocking work separately bounded.
