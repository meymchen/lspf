# lspf

[![crates.io](https://img.shields.io/crates/v/lspf.svg)](https://crates.io/crates/lspf)
[![docs.rs](https://docs.rs/lspf/badge.svg)](https://docs.rs/lspf)
[![License: MIT OR Apache-2.0](https://img.shields.io/crates/l/lspf)](#许可证)

[English](./README.md) | [简体中文](./README.zh-CN.md)

一个用于构建可扩展 LSP（Language Server Protocol，语言服务器协议）语言服务器的 Rust 框架。

`lspf` **仅支持异步模式**，目标是让开发者用很少的代码即可启动一个可工作的语言服务器。
你在 `Server` 上注册带类型的处理器，再把它交给传输层，协议本身由框架负责：生命周期、
文档同步、取消、有界并发、`tracing` span，以及通过 `Client` 发出的带类型服务端消息。

> **当前状态：** 仍处于早期阶段。本仓库的当前接口为 **0.3** —— 覆盖稳定 LSP 3.17
> 功能的封闭（sealed）功能目录、Command、多根 `Workspace`、基于 `FileProvider`
> 的未打开文件查找，以及可配置的文档同步，下面的示例均基于该接口。0.3 尚未发布：
> crates.io 上仍是 **0.2**，0.3 相对 0.2 的新增内容见[变更日志](./CHANGELOG.md)。
> 0.2 中已实现的标准功能为 hover、completion 和 Command；内置 TCP、WebSocket 和
> WASM worker 传输仍在规划中。

## 快速开始

```rust,no_run
use std::sync::Arc;

use lspf::types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Hover, HoverContents, HoverParams, MarkedString,
};
use lspf::{CancellationToken, Context, LspError, OsFileProvider, Server};

/// 只存放你自己的应用状态——文档、workspace 和 client 由框架持有，
/// 并通过 `Context` 交给处理器。
struct State;

/// 一个标准的带类型功能。`features::hover()` 描述符一次性确定协议方法、
/// 本处理器的参数与结果类型，以及服务器将宣告的 `hoverProvider` capability。
async fn hover(
    _state: Arc<State>,
    ctx: Context,
    params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let Some(document) = ctx.documents().get(&uri) else {
        return Ok(None);
    };
    Ok(Some(Hover {
        contents: HoverContents::Scalar(MarkedString::String(format!(
            "{} words",
            document.text().split_whitespace().count()
        ))),
        range: None,
    }))
}

/// 第二个带类型功能；这里提供的选项正是生成的 `completionProvider`
/// 所宣告的内容。
async fn complete(
    _state: Arc<State>,
    _ctx: Context,
    _params: CompletionParams,
    _ct: CancellationToken,
) -> Result<Option<CompletionResponse>, LspError> {
    Ok(Some(CompletionResponse::Array(vec![CompletionItem {
        label: "hello".into(),
        kind: Some(CompletionItemKind::TEXT),
        ..CompletionItem::default()
    }])))
}

/// 一个带类型的 Command，在 `workspace/executeCommand` 下按名称分发。
/// 注册会把名称按注册顺序加入生成的 `executeCommandProvider`。
async fn roots(
    _state: Arc<State>,
    ctx: Context,
    _args: Vec<String>,
    _ct: CancellationToken,
) -> Result<Vec<(String, String)>, LspError> {
    Ok(ctx
        .workspace()
        .roots()
        .into_iter()
        .map(|folder| (folder.uri.as_str().to_string(), folder.name))
        .collect())
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    // 日志写往 stderr：stdout 只承载 LSP 协议流量。
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let server = Server::builder(State)
        // 未打开的 `file:` URI 通过该 provider 从磁盘解析。
        .file_provider(OsFileProvider::new())
        .feature(lspf::features::hover(), hover)
        .feature(
            lspf::features::completion(CompletionOptions {
                trigger_characters: Some(vec![".".to_string()]),
                ..CompletionOptions::default()
            }),
            complete,
        )
        .command("hello.roots", roots)
        .build()
        .expect("the static registrations are valid");
    // serve 返回连接的结束方式（Outcome），由二进制自己决定它对进程意味着什么。
    let outcome = lspf::stdio(server).serve().await?;
    std::process::exit(outcome.code());
}
```

这里没有手写的 `ServerCapabilities`，也不需要改动分发器：capability 由注册本身
生成。完整旅程——hover、completion 及 resolve、Command、文档同步、多根 workspace
状态和未打开文件查找——的可运行版本位于
[`crates/lspf-hello/src/main.rs`](./crates/lspf-hello/src/main.rs)，它也是
[编辑器配置](#编辑器配置)中使用的模板服务器，旁边还有端到端 stdio 测试。
[功能、capability 与 workspace](./docs/guides/features-and-workspace.md)
指南逐项讲解各个部分。

## 安装依赖

```toml
[dependencies]
lspf = "0.2"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

`0.2` 是最新的已发布版本；上面的快速开始针对本仓库当前的 0.3 接口。
在 0.3 发布之前，可以直接依赖仓库：

```toml
[dependencies]
lspf = { git = "https://github.com/meymchen/lspf" }
```

`0.1.x` 是旧的 `LanguageServer` trait 接口，已被 0.2 移除，两者的对应关系
参见[迁移指南](./docs/migrations/0.1-to-0.2.md)。

`lspf` 的 `Cargo.toml` 已经引入 `lsp-types`、`tokio`、`tracing`、`serde`
等运行时依赖，因此你的应用只需为直接使用的 `tokio` 功能选择相应 feature。

## 为什么选择 lspf

- **异步优先。** 框架端到端使用 `async fn`，没有同步处理路径。
- **最小可用服务器。** 在 `Server::builder` 上注册处理器，把构建出的 `Server`
  交给 `lspf::stdio(...)`，即可得到一个可工作的 LSP 服务器。
- **由框架管理文档状态。** 增量文本变更会在你的钩子运行前应用到框架持有的、
  并发安全且基于 rope 的 `Documents`；处理器通过没有任何修改操作的
  `DocumentsView` 读取它们。
- **多根 `Workspace`。** 客户端声明——文件夹、根 URI、配置、trace 级别——存放在
  一个可克隆的句柄中，只由协议修改，通过 `Context` 读取；未打开文件通过可配置的
  `FileProvider` 解析。
- **capability 不会与分发脱节。** `ServerCapabilities` 由参与分发的同一份注册
  生成，因此服务器宣告的能力就是它实际提供的能力；互相冲突的注册是构建错误，
  绝不会静默地以后注册者为准。
- **安全的并发分发。** 请求和通知受可配置的并发上限约束（默认 64）；
  `$/cancelRequest` 通过 `CancellationToken` 传播。
- **代为处理协议细节。** 生命周期顺序、JSON-RPC framing、文本同步以及
  UTF-8/UTF-16 位置编码协商均由框架处理。
- **可替换传输。** 框架内置 `stdio`；也可以实现公开的 `Transport` trait，
  将 lspf 嵌入测试或其他消息通道。

## 核心概念

以下术语与当前公开 API 对应：

| 术语                | 含义                                                                                   |
| ------------------- | -------------------------------------------------------------------------------------- |
| `Server`            | 持有恰好一个 LSP 连接；由 `Server::builder(state)` 构建，并在 `Transport` 上运行。     |
| Handler             | 为某个 LSP 方法注册的异步函数；用户处理器优先于内置处理器。                            |
| 内置处理器          | 框架自带的处理器：生命周期、文档同步和取消都是协议内置项。                             |
| 变更后钩子          | 为内置文档通知注册处理器得到的结果：它观察变更，而不会替换变更。                       |
| `Command`           | 通过 `workspace/executeCommand` 按名称分发的用户闭包。                                 |
| `Document`          | 框架跟踪的文本资源：URI、语言 ID、版本和基于 rope 的内容。                             |
| `DocumentsView`     | 处理器通过 `ctx.documents()` 获得的只读文档句柄。                                      |
| `Workspace`         | 连接 workspace 状态的可克隆句柄：文件夹、配置和文档。                                  |
| `FileProvider`      | 解析编辑器中未打开资源的可配置 provider。                                              |
| `Context`           | 每个处理器都会收到的框架状态句柄（克隆开销极小）：文档、workspace 和 client。          |
| `Client`            | 发送带类型服务端通知与请求的句柄（`ctx.client()`）。                                   |
| `CancellationToken` | 传递给请求处理器的取消信号。                                                           |
| `Transport`         | 供协议引擎使用、拆分为 reader 和 writer 两部分的消息帧通道。                           |
| `Outcome`           | 连接如何结束；serve 返回它，其中带有 LSP 退出码，但框架从不结束进程。                  |

## 架构

完整设计文档与代码一起维护：

- [`CONTEXT.md`](./CONTEXT.md)：领域语言和共享词汇。
- [`docs/adr/`](./docs/adr/)：24 份架构决策记录，涵盖纯异步运行时、带类型的 Router
  与 capability 目录、协议引擎与出站请求代理、取消模型、传输形式、`Layer`/`Service`
  栈、位置编码等。ADR 同时描述架构方向和已经交付的行为；ADR 被接受并不表示对应
  功能已经实现。
- [`docs/guides/features-and-workspace.md`](./docs/guides/features-and-workspace.md)：
  如何注册功能、capability 从何而来、谁持有 workspace 和文档、Command 如何分发，
  以及 `FileProvider` 的配置方式。其中的每个示例都作为 doctest 编译。

## 路线图

当前已经可用：

- `stdio` 和公开的自定义传输接口。
- 构建出的 `Server`：带类型的请求、通知、Command、覆盖稳定 LSP 3.17 功能的封闭
  功能目录、用户 `Layer`，以及唯一的 `configure_initialize` 事务。
- 生命周期、增量或全量文本文档同步，以及变更后文档钩子。
- 多根 `Workspace`、最新配置设置，以及基于 `FileProvider` 的未打开文件查找。
- 通过 `Client` 发送带类型的服务端通知与可关联响应的请求。
- 并发分发、有界并发、请求取消和 `tracing` span。
- 基于 rope 的文档，以及 UTF-8/UTF-16 位置编码协商。

已有规划，但尚未承诺发布版本：

- 内置 TCP、WebSocket 和 WASM worker 传输。

## 示例

可以直接在 workspace 中运行模板服务器，也可以让任何 LSP 客户端启动该进程：

```bash
cargo run -p lspf-hello
```

它就是完整的带类型旅程——hover、completion 及 resolve、Command、文档同步、
多根 workspace 状态和未打开文件查找——由
[`crates/lspf-hello/tests/e2e.rs`](./crates/lspf-hello/tests/e2e.rs)端到端验证。
若要连接真实编辑器，请参阅[编辑器配置](#编辑器配置)。

## 编辑器配置

本仓库是包含两个成员的 Cargo workspace：

- [`crates/lspf`](./crates/lspf)：应用依赖的框架库（`lspf = "0.2"`）。
- [`crates/lspf-hello`](./crates/lspf-hello)：可安装的**模板服务器**。它生成通过
  stdio 使用 LSP 的 `lspf-hello` 二进制：应答 hover 与 completion（含 resolve），
  分发 `lspf-hello.workspaceRoots` 与 `lspf-hello.readFile` 两个 Command，
  通过 `OsFileProvider` 读取未打开文件；每次收到 `textDocument/didOpen` 时，
  都会发布一条 “lspf saw this document open” 信息级诊断。你可以 fork 它作为自己
  语言服务器的起点。

### 安装服务器

```bash
cargo install --path crates/lspf-hello
```

该命令会把 `lspf-hello` 安装到 Cargo 的二进制目录（默认为 `~/.cargo/bin`）。
请确保这个目录位于 `PATH` 中，以便编辑器按名称启动服务器。

### VS Code

VS Code 没有内置的通用 LSP 客户端，因此需要安装轻量的通用客户端扩展，例如
[Generic LSP Client (v2)](https://marketplace.visualstudio.com/items?itemName=zsol.vscode-glspc)，
然后在 `settings.json` 中加入：

```json
{
  "glspc.server.command": "lspf-hello",
  "glspc.server.commandArguments": [],
  "glspc.server.languageId": ["plaintext"]
}
```

打开任意纯文本（`.txt`）文件后，应能在第一行看到
“lspf saw this document open” 诊断。

> 开发框架时可以跳过安装，改用仓库内置的
> [`tools/vscode-test-client`](./tools/vscode-test-client)。它会直接启动
> `target/` 中刚刚构建的二进制。

### Zed

Zed 目前要求语言扩展预先注册每个 language-server adapter。
`lsp.<name>.binary` 设置可以覆盖 Zed 已知 adapter 的可执行文件，但不能只通过
`settings.json` 注册 `lspf-hello` 这样的任意新服务器。

本仓库暂未提供 Zed 扩展。可以参考 Zed 的
[语言扩展文档](https://zed.dev/docs/extensions/languages)创建注册 `lspf-hello`
的开发扩展，或者使用上面的 VS Code 测试客户端完成仓库支持的编辑器冒烟测试。

### 故障排除

- **找不到 `lspf-hello` / “command not found”。** 二进制不在 `PATH` 中。
  用 `which lspf-hello` 确认；如果无法解析，请把 `~/.cargo/bin` 添加到 `PATH`，
  或在编辑器配置中使用绝对路径。
- **服务器未启动或没有出现诊断。** 确保修改代码后重新执行了
  `cargo install --path crates/lspf-hello`，并确认编辑器客户端会把当前文件路由给
  这个服务器。示例编辑器配置以纯文本文件为目标；服务器本身不会按语言 ID 过滤
  `didOpen`。可以在终端中用 `RUST_LOG=lspf=trace` 运行 `lspf-hello`，确认它能够
  启动并在 stderr 中查看 LSP 流量。
- **修改配置后没有变化。** 编辑器会在启动时读取 LSP 设置。修改 `settings.json`
  后请重新加载窗口（VS Code：*Developer: Reload Window*；Zed：重新打开 workspace）。

## 参与贡献

Issue 位于 GitHub 仓库
[meymchen/lspf](https://github.com/meymchen/lspf/issues)，并通过 `gh` 管理。
分类使用固定标签：`needs-triage`、`needs-info`、`ready-for-agent`、
`ready-for-human`、`wontfix`，方便 agent 或开发者直接接手。

提交 PR 前，请先浏览：

- [`CONTEXT.md`](./CONTEXT.md)：确认修改符合项目词汇。
- 相关的 `docs/adr/*.md`：如果修改重新讨论了已有决策，请在 PR 描述中解释偏离原因，
  或新增一份 ADR。

使用仓库共享配置检查全部 Markdown（Node.js 24）：

```bash
npx --yes markdownlint-cli2@0.22.1
```

大多数机械性的 Markdown 问题可以先自动修复，再人工检查结果：

```bash
npx --yes markdownlint-cli2@0.22.1 --fix
```

生成本地 HTML 覆盖率报告：

```bash
cargo install cargo-llvm-cov --version 0.6.21 --locked
cargo coverage
```

然后打开 `target/coverage/html/index.html`。CI 也会在每个 PR 和 `main` push
中上传覆盖率报告 artifact。

## 许可证

你可以任选以下许可证之一使用本项目：

- [Apache License, Version 2.0](./LICENSE-APACHE)
- [MIT License](./LICENSE-MIT)
