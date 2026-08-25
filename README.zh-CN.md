# lspf

[![crates.io](https://img.shields.io/crates/v/lspf.svg)](https://crates.io/crates/lspf)
[![docs.rs](https://docs.rs/lspf/badge.svg)](https://docs.rs/lspf)
[![License: MIT OR Apache-2.0](https://img.shields.io/crates/l/lspf)](#许可证)

[English](./README.md) | [简体中文](./README.zh-CN.md)

一个用于构建可扩展 LSP（Language Server Protocol，语言服务器协议）语言服务器的 Rust 框架。

`lspf` **仅支持异步模式**，目标是让开发者用很少的代码即可启动一个可工作的语言服务器。
你在 `Server` 上注册带类型的处理器，再把它交给传输层，协议本身由框架负责：生命周期、
文档同步、取消、有界并发、`tracing` span，以及通过 `Client` 发出的带类型服务端消息。

> **当前状态：** 项目仍处于早期阶段。当前已发布版本为 **0.5.2**。该版本包含
> 稳定 LSP 3.17 功能目录、带类型的 Command、多根 `Workspace`、出站 `Client`
> 辅助方法，以及 stdio、TCP、WebSocket 和 WASM worker-channel 传输。版本历史见
> [crate 变更日志](./crates/lspf/CHANGELOG.md)。

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
[功能、capability 与 workspace](./docs/guides/features-and-workspace.zh-CN.md)
指南逐项讲解各个部分。
传输选择、Cargo feature 组合和自定义消息通道见
[传输指南](./docs/guides/transports.zh-CN.md)。
各项 LSP 功能的可运行示例服务见
[`crates/lspf/examples/README.zh-CN.md`](./crates/lspf/examples/README.zh-CN.md)。

## 安装依赖

```toml
[dependencies]
lspf = "0.5.2"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

该 crate 要求 Rust 1.96 或更高版本。快速开始使用默认启用的 `stdio` feature；
如需其他传输，请选择对应的 feature 组合。

应用代码直接引用的 crate 都应列为直接依赖。上例中的 `tokio`、`tracing` 和
`tracing-subscriber` 不能因为 lspf 内部也使用它们而省略。

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
- **内置与自定义传输。** 框架内置 `stdio`、单客户端 TCP、单客户端 WebSocket
  与 WASM worker-channel adapter；也可以实现公开的 `Transport` trait，将 lspf
  嵌入测试或其他消息通道。

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
- [`docs/adr/`](./docs/adr/)：架构决策记录，涵盖纯异步运行时、带类型的 Router
  与 capability 目录、协议引擎与出站请求代理、取消模型、传输形式、`Layer`/`Service`
  栈、位置编码等。ADR 同时描述架构方向和已经交付的行为；ADR 被接受并不表示对应
  功能已经实现。
- [`docs/guides/features-and-workspace.zh-CN.md`](./docs/guides/features-and-workspace.zh-CN.md)：
  如何注册功能、capability 从何而来、谁持有 workspace 和文档、Command 如何分发，
  以及 `FileProvider` 的配置方式。其中的每个示例都作为 doctest 编译。
- [`docs/guides/outgoing-client.zh-CN.md`](./docs/guides/outgoing-client.zh-CN.md)：
  服务器到客户端的辅助方法全景：通知、窗口与 workspace 请求、动态注册、
  workspace 刷新和 work-done 进度，并附完整的辅助方法参考表。其中的每个示例都作为
  doctest 编译。
- [`docs/guides/transports.zh-CN.md`](./docs/guides/transports.zh-CN.md)：传输选择、target/feature
  兼容矩阵、原生与 WASM 示例，以及自定义 `Transport` 契约。
- [`SECURITY.zh-CN.md`](./SECURITY.zh-CN.md)：受支持的 Rust 版本、host、target、
  Cargo feature 组合、版本兼容性、弃用规则和私密漏洞报告方式。

## 当前范围

当前已经可用：

- `stdio`、单客户端 TCP、WebSocket 与 WASM worker-channel adapter，以及公开的
  自定义传输接口。
- 构建出的 `Server`：带类型的请求、通知、Command、覆盖稳定 LSP 3.17 功能的封闭
  功能目录、用户 `Layer`，以及唯一的 `configure_initialize` 事务。
- 覆盖 shutdown 与 exit 的生命周期钩子、增量或全量文本文档同步，以及变更后
  文档钩子。
- 多根 `Workspace`、最新配置设置，以及基于 `FileProvider` 的未打开文件查找。
- 通过 `Client` 发送带类型的服务端通知与可关联响应的请求。
- 并发分发、有界并发、请求取消和 `tracing` span。
- 基于 rope 的文档，以及 UTF-8/UTF-16 位置编码协商。

## 示例

原生与 WASM 传输示例共用同一套处理器。TCP、WebSocket、browser Worker 和
Node Worker 的构建命令见[传输指南](./docs/guides/transports.zh-CN.md)。

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

- [`crates/lspf`](./crates/lspf)：应用依赖的框架库（`lspf = "0.5.2"`）。
- [`crates/lspf-hello`](./crates/lspf-hello)：可安装的**模板服务器**。它生成通过
  stdio 使用 LSP 的 `lspf-hello` 二进制：应答 hover 与 completion（含 resolve），
  分发用于 workspace root、文件读取、出站 client 辅助方法和可取消进度的四个
  Command，通过 `OsFileProvider` 读取未打开文件；每次收到
  `textDocument/didOpen` 时，
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
> [`tools/vscode-test-client/README.zh-CN.md`](./tools/vscode-test-client/README.zh-CN.md)。它会直接启动
> `target/` 中刚刚构建的二进制。

#### 仓库开发

在 VS Code 中打开仓库根目录，并安装推荐的 rust-analyzer 与 CodeLLDB 扩展。仓库中的
`.vscode` 配置提供以下入口：

- `Debug LSP client (Extension Host)` 是默认端到端路径。它会构建 `lspf-hello`、
  根据 lock file 安装缺少的测试客户端依赖、编译客户端，再打开 Extension
  Development Host。在其中打开 `.txt` 文件即可测试服务器。
- `Run LSP example client (select example)` 会构建 stdio 示例，让你选择一个示例，
  再打开由该示例真实进程支持的 Extension Development Host。
- `Attach to running LSP server/example` 使用 CodeLLDB 连接由上述任一客户端配置
  启动的进程。
- 另外还提供 build、quick test、完整 workspace test 和示例运行 task。quick test
  是 `cargo test -p lspf-hello`，完整 task 与 CI 的主要测试命令相同。

调试示例时，先运行 `Run LSP example client (select example)`，并选择 `hover` 等
示例。在新 Extension Development Host 中打开 `.txt` 文件，再回到仓库窗口运行
`Attach to running LSP server/example`，选择以该示例命名的进程，然后在
`crates/lspf/examples/<name>.rs` 中设置 breakpoint。编辑器操作会通过真实 stdio
连接到达 Rust 处理器。

除非环境中已有对应值，Extension Host debug 配置默认使用 `RUST_LOG=lspf=trace` 和
`LSPF_LOG_FORMAT=json`。stderr 每行包含一个 JSON event，其中带有 event field 与
当前 span。启动 VS Code 前设置 `LSPF_LOG_FORMAT=text` 可改用紧凑文本。run 与 test
task 不会修改这两个变量。

使用 `lspf=trace` 时，框架会输出五种稳定 event shape。入站与出站流量使用相同的
field 名称：

| `message` | Field |
| --- | --- |
| `rpc message` | `connection_id`、`direction`、`kind`，以及存在时的 `method` 与 `request_id` |
| `resource budget changed` | `connection_id`、`resource`、`resource_action`、`resource_current`；有上限的资源还包括 `resource_limit`，字节预算还包括 `resource_bytes` 与 `resource_bytes_limit`，`pending_requests` 还包括 `direction`、`kind`、`method`、`request_id` 和可选 `deadline_ms` |
| `deadline changed` | `connection_id`、`direction`、`kind`、`method`、`request_id`、`deadline`、`deadline_action`、`deadline_ms`、`deadline_elapsed_ms` |
| `request completed` | `connection_id`、`direction`、`kind`、`method`、`request_id`、`latency_ms`、`completion` |
| `connection closed` | `connection_id`、`close_cause` |

`direction` 的值为 `inbound` 或 `outbound`。资源名为 `inbound_requests`、
`outbound_queue`、`documents` 与 `pending_requests`。deadline 名为 `handler` 与
`outbound_request`。resource action 的值为 `admit`、`release`、`update`、`reject`
与 `rollback`。deadline action 的值为 `armed`、`completed`、`cancelled` 与
`expired`。completion 的值为 `success`、`error`、`cancelled`、
`deadline_expired`、`rejected` 与 `connection_closed`；close cause 的值为 `exit`、
`reader_eof`、`reader_failed`、`writer_failed` 与 `initialize_failed`。可选 field
不存在时会直接省略，不会写入哨兵值。

request 与 notification span 使用相同的 `connection_id`、`direction`、`kind`、
`method` 和可选 `request_id` field。request span 还会保留原有的 debug-formatted
`id` field，以兼容现有 consumer。因此，handler 写入的 event 会通过当前 span 继承
connection 与 call identity。

默认 event 不会记录 request parameter、response result、Document text 或序列化后的
wire envelope。应用可以在 handler 内添加自己的 event，但也应把这些 payload 视为
敏感数据。

如果 metrics 或 alerting 不应依赖 tracing 输出，可在 Server 上注册一个连接错误
hook：

```rust
struct State;

let _server = lspf::Server::builder(State)
    .on_error(|failure| {
        eprintln!(
            "connection {}: {:?}",
            failure.context.connection_id,
            failure.category,
        );
    })
    .build()
    .expect("server configuration is valid");
```

`ConnectionFailureCategory` 区分 framing、protocol、Transport、panic-isolation、
overload 与 close failure。context 包含 connection ID，以及已知的 direction、method
与 request ID；不会包含 parameter、result、Document text、wire data、panic payload
或底层 error message。numeric request ID 会保留原值；peer-controlled string ID
只会显示为 `ConnectionRequestId::String`，不会暴露内容。method name 只有在它是
framework-owned、已注册，或由 typed outbound request 在本地声明时才会包含；其他
peer-controlled method name 会被省略。每个 failure 都在来源处
报告一次。hook 内的 panic 会被捕获并记录，不能阻止 response 或中断连接清理。
该 hook 在用户 Layer chain 外观察连接 failure；用户 Layer 仍然只包装用户 dispatch。

服务器初始化后，`vscode-languageclient` 会自动注册 `executeCommandProvider` 宣告的
四个 Command。扩展 manifest 在 Command Palette 的 `lspf hello` 分类下提供标题。
middleware 会为 `Read Active File` 和 `Run Outgoing Helper Journey` 加入当前编辑器的
URI，并把结果写入 `lspf-hello commands` output channel。outgoing journey 会调用
`workspace/applyEdit`，在当前文档开头插入注释。

### Zed

Zed 目前要求语言扩展预先注册每个 language-server adapter。
`lsp.<name>.binary` 设置可以覆盖 Zed 已知 adapter 的可执行文件，但不能只通过
`settings.json` 注册 `lspf-hello` 这样的任意新服务器。

本仓库暂未提供 Zed 扩展。可以参考 Zed 的
[语言扩展文档](https://zed.dev/docs/extensions/languages)，创建注册 `lspf-hello`
的开发扩展，或者使用上面的 VS Code 测试客户端完成仓库支持的编辑器冒烟测试。

仓库中的 `.zed/tasks.json` 提供 build、quick test、完整 workspace test 和 `hover`
示例 task。`.zed/debug.json` 中的 `Attach to running LSP server/example` 会打开 Zed
进程选择器，并通过 CodeLLDB 连接正在运行的服务器。连接前，请先通过上面的 VS Code
测试客户端、其他 LSP client 或本地 Zed 语言扩展启动进程。这些 Zed 文件用于 Rust
调试，不会把 `lspf-hello` 注册为 Zed language server。

### 故障排除

- **找不到 `lspf-hello` / “command not found”。** 二进制不在 `PATH` 中。
  用 `which lspf-hello` 确认；如果无法解析，请把 `~/.cargo/bin` 添加到 `PATH`，
  或在编辑器配置中使用绝对路径。
- **服务器未启动或没有出现诊断。** 确保修改代码后重新执行了
  `cargo install --path crates/lspf-hello`，并确认编辑器客户端会把当前文件路由给
  这个服务器。示例编辑器配置以纯文本文件为目标；服务器本身不会按语言 ID 过滤
  `didOpen`。可以在终端中运行
  `RUST_LOG=lspf=trace LSPF_LOG_FORMAT=json lspf-hello`，确认它能够启动，并在
  stderr 中输出 newline-delimited JSON 日志。
- **修改配置后没有变化。** 编辑器会在启动时读取 LSP 设置。修改 `settings.json`
  后请重新加载窗口（VS Code：*Developer: Reload Window*；Zed：重新打开 workspace）。
- **直接运行后看起来卡住。** `lspf-hello` 和框架示例通过 stdio 等待 LSP
  客户端。请让 VS Code 测试客户端启动进程，或运行 quick test 完成自动检查。

## 参与贡献

Issue 位于 [GitHub tracker](https://github.com/meymchen/lspf/issues)。

提交 PR 前，请先浏览：

- [`CONTEXT.md`](./CONTEXT.md)：确认修改符合项目词汇。
- 相关的 `docs/adr/*.md`：如果修改重新讨论了已有决策，请在 PR 描述中解释偏离原因，
  或新增一份 ADR。

手写的用户向文档需要同时维护中英文版本。每份英文 README 或 guide 都应有对应的
`.zh-CN.md` 文件；自动生成的发布文档除外。

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
