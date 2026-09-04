---
title: 构建语言服务器
description: 从零开始构建一个完整的类型安全 stdio 服务器。
---

本教程会构建一个 stdio 服务器：它可以响应悬停请求，并读取由 lspf 维护的文档状态。

## 1．创建 crate

```console
cargo new lspf-tutorial-server
cd lspf-tutorial-server
```

```toml title="Cargo.toml"
[package]
name = "lspf-tutorial-server"
version = "0.1.0"
edition = "2024"

[dependencies]
lspf = "1.0.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## 2．定义应用状态

框架负责文档、工作区状态和客户端连接。你的状态只保存应用自身的数据：

```rust
struct State {
    product_name: &'static str,
}
```

## 3．注册类型化处理器

```rust
use std::sync::Arc;
use lspf::types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};
use lspf::{CancellationToken, LspError, ServerContext};

async fn hover(
    state: Arc<State>,
    ctx: ServerContext,
    params: HoverParams,
    _cancellation: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let uri = &params.text_document_position_params.text_document.uri;
    let Some(document) = ctx.documents().get(uri) else {
        return Ok(None);
    };

    Ok(Some(Hover {
        contents: HoverContents::MarkupContent(MarkupContent {
            kind: MarkupKind::PlainText,
            value: format!("{} · {} words", state.product_name,
                document.text().split_whitespace().count()),
        }),
        range: None,
    }))
}
```

返回 `Ok(None)` 表示“没有结果”。只有需要让编辑器收到错误响应时才使用 `LspError`。

## 4．响应文档变更

`textDocument/didOpen` 和 `textDocument/didChange` 是协议内置消息：引擎先解码并更新文档，再调用你注册的变更后钩子。钩子观察的是已经更新的不可变快照，不能替换框架更新。

在打开和变更钩子中读取 `ctx.documents()`，找出超过 `State` 行宽限制的行，并通过 `ctx.publish_diagnostics` 发送带文档版本的诊断。范围位置必须使用 `ctx.documents().position_encoding()` 及转换辅助方法计算；UTF-8 字节索引不能直接当作 UTF-16 或 UTF-32 位置。关闭文档时发送空诊断，避免客户端保留旧结果。

通知处理器没有可返回给对端的错误响应。可恢复问题应记录到 stderr 或应用遥测；资源接纳或版本校验失败时，框架会保留旧快照且不运行钩子。

## 5．公开 Command

用 `.command("tutorial.countWords", handler)` 注册命名命令。构建器会把名称加入 `executeCommandProvider.commands`，并把 `workspace/executeCommand` 分派给处理器。处理器应验证参数数量和 URI 类型，从 `ctx.documents()` 读取当前快照，检查取消令牌，再返回 JSON 值。参数无效或文档不存在时返回适合用户的 `LspError`，不要 panic。

## 6．构建服务器并提供服务

```rust
use lspf::Server;

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State { product_name: "My LS" })
        .feature(lspf::features::hover(), hover)
        .build()?;

    let outcome = lspf::stdio(server).serve().await?;
    std::process::exit(outcome.code());
}
```

标准输出只用于 LSP 消息帧。请把 tracing 日志写入标准错误。

## 7．组合完整程序

把 `State`、hover 处理器、文档打开／变更钩子、诊断辅助函数和命令处理器放入 `src/main.rs`。构建器依次注册 hover、带增量 `TextDocumentSyncOptions` 的文档钩子和 `tutorial.countWords` 命令，再调用 `build()`。初始化 tracing 时使用 stderr writer；stdout 必须只承载 LSP 消息帧。

`serve` 返回 `Outcome`，但 lspf 不会自行终止进程。顶层 `main` 应把错误返回给宿主，或在完成清理后使用 `outcome.code()` 决定退出码。

## 8．从终端驱动

先构建服务器和[客户端教程](../client/)中的客户端，再把服务器可执行文件的绝对路径传给客户端：

```console
cargo run -- /absolute/path/to/lspf-tutorial-server
```

客户端会完成 initialize／initialized，打开文档，断言一条超长行诊断，请求返回 `11 words` 的 hover，执行 `tutorial.countWords` 并断言结果为 11，随后发送 shutdown／exit 并检查进程状态。没有输出表示全部断言通过。

## 9．连接编辑器

编辑器插件应以 stdio 子进程启动服务器，把服务器能力作为 initialize 响应读取，并把日志保留在 stderr。不要在服务器内部硬编码重启或 UI 策略；这些属于启动它的编辑器。调试时可设置 `RUST_LOG=lspf=trace`，但仍不能把日志写到 stdout。

## 下一步

- [功能注册](../../guides/features-and-workspace/)介绍更多功能描述符；[工作区状态](../../guides/workspace-state/)介绍文档、笔记本和文件提供器。
- [调用编辑器](../../guides/outgoing-client/)介绍诊断、配置、刷新和工作区编辑；[进度与自定义消息](../../guides/progress-and-custom-messages/)介绍长时间运行的工作和协议扩展。
- [错误与取消](../../guides/errors-and-cancellation/)介绍可见错误、取消令牌和阻塞工作。
- [资源与可观测性策略](../../guides/operations/)介绍资源预算和遥测；[部署与故障排查](../../guides/deployment-and-troubleshooting/)介绍关闭策略和进程拓扑。
