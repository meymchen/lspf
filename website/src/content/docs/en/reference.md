---
title: API reference
description: Find versioned API documentation and project reference material.
---

Use the versioned Rust API reference for exact signatures and feature availability.

## Rust API

- [Latest lspf API on docs.rs](https://docs.rs/lspf)
- [Crate releases on crates.io](https://crates.io/crates/lspf)
- [Frozen 1.0 public interface](https://github.com/meymchen/lspf/blob/main/docs/public-interface.md)

## Task guides

- [Errors and cancellation](guides/errors-and-cancellation)
- [Register server features](guides/features-and-workspace)
- [Manage workspace state](guides/workspace-state)
- [Call the editor](guides/outgoing-client)
- [Report progress and custom messages](guides/progress-and-custom-messages)
- [Choose a transport](guides/transports)
- [Use stdio and custom transports](guides/stdio-and-custom-transports)
- [Build an LSP client](guides/client-adoption)
- [Protocol testing](guides/testing)
- [Resource and observability policies](guides/operations)
- [Deployment and troubleshooting](guides/deployment-and-troubleshooting)
- [Feature example servers](examples)

## Architecture and support

- [Domain model](https://github.com/meymchen/lspf/blob/main/CONTEXT.md)
- [Architecture decisions](https://github.com/meymchen/lspf/tree/main/docs/adr)
- [Support and security policy](https://github.com/meymchen/lspf/blob/main/SECURITY.md)
- [Release history](https://github.com/meymchen/lspf/blob/main/crates/lspf/CHANGELOG.md)

::: info Release awareness
The repository may contain APIs scheduled for the next release. Use the docs.rs page for
the version in your `Cargo.lock` when you need the exact published surface.
:::
