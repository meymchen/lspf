---
title: 开始使用
description: 安装 lspf，并运行一个类型安全的 stdio 语言服务器。
---

`lspf` 是一个用于实现语言服务器协议（LSP）服务器与客户端的异步 Rust 框架。本指南会创建一个最小但实用的 stdio 服务器。

## 环境要求

- Rust 1.98 或更高版本。
- 了解 `async fn` 和 Cargo 的基本用法。

## 安装

创建二进制 crate，并添加包含默认 `stdio` 传输层的 lspf：

```console
cargo new my-language-server
cd my-language-server
cargo add lspf
cargo add tokio --features macros,rt-multi-thread
```

默认功能集已经包含 stdio。如果应用使用 TCP、WebSocket 或 Worker Channel，请显式选择对应的传输功能。

## 构建服务器

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

`hover()` 描述符同时确定了协议方法、参数类型、结果类型和对外声明的能力，不需要再维护一份独立的 `ServerCapabilities`。

## 接下来

- 跟随[构建语言服务器](tutorials/server)教程，加入文档同步、诊断和命令。
- 阅读[核心概念](concepts)，理解 lspf 的所有权模型。
- 在 [docs.rs](https://docs.rs/lspf) 查看完整且带版本的 API。
