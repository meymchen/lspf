---
title: lspf — Language servers, without the protocol plumbing
description: Build typed, async Language Server Protocol servers and clients in Rust.
layout: home
editLink: false
lastUpdated: false
hero:
  name: lspf
  text: Language servers, without the protocol plumbing.
  tagline: A typed, async Rust framework that owns the LSP lifecycle, documents, capabilities, cancellation, and transport — so you can focus on language features.
  actions:
    - theme: brand
      text: Start building
      link: /getting-started
    - text: View on GitHub
      theme: alt
      link: https://github.com/meymchen/lspf
features:
  - icon: 🧩
    title: Typed from handler to wire
    details: Register protocol features with their Rust types. lspf derives advertised capabilities from those same registrations, keeping behavior and metadata aligned.
  - icon: 📄
    title: Protocol state included
    details: Read synchronized documents, notebooks, workspace roots, configuration, and the client connection through the context supplied to each handler.
  - icon: 🚀
    title: Async, bounded, cancellable
    details: Serve concurrent requests with finite resource policies and cancellation tokens, using an async-first API designed for production ownership.
  - icon: 🔌
    title: Bring the right transport
    details: Start with stdio, TCP, WebSocket, or browser and Node workers. Implement the public message-framed transport traits when your host needs something else.
---

## A small API with a clear boundary

```rust
let server = Server::builder(State)
    .feature(lspf::features::hover(), hover)
    .feature(lspf::features::completion(options), complete)
    .command("acme.organize", organize)
    .build()?;

let outcome = lspf::stdio(server).serve().await?;
```

The framework handles JSON-RPC, initialization, document synchronization, capability
advertisement, cancellation, and shutdown. Your handlers receive typed parameters and
return typed results.

[Install lspf and build your first server →](./getting-started)

## Find the right kind of documentation

- **New to lspf?** Start with [Getting started](./getting-started), then build
  the complete [tutorial server](./tutorials/server).
- **Building a server?** Start with [feature registration](./guides/features-and-workspace)
  and [workspace state](./guides/workspace-state), then add
  [editor calls](./guides/outgoing-client) or
  [progress reporting](./guides/progress-and-custom-messages) as needed.
- **Connecting or shipping it?** Choose a [transport](./guides/transports), then
  use the focused guides for [client connections](./guides/client-adoption),
  [testing](./guides/testing), and [production policies](./guides/operations).
- **Looking for working code?** Pick one of the small
  [feature example servers](./examples).
- **Need an exact signature?** Open the versioned [API reference](./reference).

## What lspf supports

The stable feature catalog covers LSP 3.18 requests and notifications. Native servers
can use stdio, TCP, or WebSocket. Browser and Node workers use the worker-channel
adapter, and embedded hosts can implement the public transport traits. The framework
also includes a typed outbound client endpoint, document and notebook synchronization,
workspace state, bounded concurrency, cancellation, progress, and protocol test tools.
