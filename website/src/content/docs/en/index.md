---
title: lspf — Language tooling for IDEs and agents
description: Build language servers for IDEs in Rust, then extend LSP language capabilities to agent tools with typed clients.
layout: home
editLink: false
lastUpdated: false
hero:
  name: lspf
  text: Built for IDEs.<br>Extended to agents.
  tagline: Build the language features your editor depends on. Use typed Rust servers for IDE integration, then connect agent tools through the same Language Server Protocol.
  actions:
    - theme: brand
      text: Start building
      link: /getting-started
    - text: View on GitHub
      theme: alt
      link: https://github.com/meymchen/lspf
features:
  - title: Typed from handler to wire
    details: Register protocol features with their Rust types. lspf derives advertised capabilities from those same registrations, keeping behavior and metadata aligned.
  - title: Protocol state included
    details: Read synchronized documents, notebooks, workspace roots, configuration, and the client connection through the context supplied to each handler.
  - title: Async, bounded, cancellable
    details: Serve concurrent requests with finite resource policies and cancellation tokens, using an async-first API designed for production ownership.
  - title: Bring the right transport
    details: Start with stdio, TCP, WebSocket, or browser and Node workers. Implement the public message-framed transport traits when your host needs something else.
---

## IDE foundations, with room for agents

Start with the IDE: implement hover, completion, diagnostics, and other language
features in a lspf server. The framework manages the LSP lifecycle, synchronized
documents, capability advertisement, and cancellation while your handlers supply
language analysis.

Extend that foundation with lspf's typed `Client`: an agent host can connect to a
language server and reuse its LSP features. Tool selection, model calls, and decisions
about applying edits remain in the host application.

<!-- markdownlint-disable-next-line MD033 -->
<ArchitectureFlow />

[Explore the server architecture](./concepts) · [Build a client connection](./guides/client-adoption)

## Built with lspf

<!-- markdownlint-disable-next-line MD033 -->
<div class="application-card">

### [lspf-analysis](./built-with-lspf)

See function and class code health as you edit. lspf-analysis brings multi-language
analysis into the editor through diagnostics, hover details, and a Code Health view.
Its Rust server uses lspf for document synchronization and LSP communication,
with application handlers connecting the analysis engine to editor features.

<!-- markdownlint-disable-next-line MD033 -->
<p class="application-status">VS Code extension · Pre-release</p>

<!-- markdownlint-disable-next-line MD033 -->
<div class="application-actions">

[Try in VS Code](https://marketplace.visualstudio.com/items?itemName=meymchen.lspf-analysis)
[How it uses lspf](./built-with-lspf)
[Source code](https://github.com/meymchen/lspf-analysis)

<!-- markdownlint-disable-next-line MD033 -->
</div>
<!-- markdownlint-disable-next-line MD033 -->
</div>

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
