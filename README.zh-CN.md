# lspf

[![crates.io](https://img.shields.io/crates/v/lspf.svg)](https://crates.io/crates/lspf)
[![docs.rs](https://docs.rs/lspf/badge.svg)](https://docs.rs/lspf)
[![License: MIT OR Apache-2.0](https://img.shields.io/crates/l/lspf)](#许可证)

[English](./README.md) | [简体中文](./README.zh-CN.md)

一个用于构建可扩展 LSP（Language Server Protocol，语言服务器协议）语言服务器的 Rust 框架。

`lspf` **仅支持异步模式**，目标是让开发者用很少的代码即可启动一个可工作的语言服务器。
当前版本提供 `stdio` 传输、生命周期和文本文档分发、并发文档存储、请求取消、有界并发、
`tracing` span，以及每个处理器 `Context` 上的 `publish_diagnostics`。

> **当前状态：** `0.1.2` 是早期版本，已经实现的接口有意保持精简：`stdio`、自定义传输、
> 生命周期处理器、文本文档同步和 `publish_diagnostics`。`Layer`/`Service` API、更多
> LSP 功能与出站辅助方法，以及内置 TCP、WebSocket 和 WASM worker 传输仍在规划中，
> 目前尚不可用。

## 快速开始

```rust
use lspf::types::{
    Diagnostic, DiagnosticSeverity, DidOpenTextDocumentParams, Position,
    PublishDiagnosticsParams, Range,
};
use lspf::{Context, Documents, LanguageServer};

struct Hello {
    documents: Documents,
}

impl Hello {
    fn new() -> Self {
        Self {
            documents: Documents::new(),
        }
    }
}

impl LanguageServer for Hello {
    fn documents(&self) -> &Documents {
        &self.documents
    }

    async fn text_document_did_open(
        &self,
        ctx: &Context,
        params: DidOpenTextDocumentParams,
    ) {
        ctx.publish_diagnostics(PublishDiagnosticsParams {
            uri: params.text_document.uri,
            version: Some(params.text_document.version),
            diagnostics: vec![Diagnostic {
                range: Range {
                    start: Position { line: 0, character: 0 },
                    end:   Position { line: 0, character: 0 },
                },
                severity: Some(DiagnosticSeverity::INFORMATION),
                source: Some("lspf-hello".into()),
                message: "lspf saw this document open".into(),
                ..Diagnostic::default()
            }],
        });
    }
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    lspf::stdio(Hello::new()).serve().await
}
```

可运行版本位于
[`crates/lspf-hello/src/main.rs`](./crates/lspf-hello/src/main.rs)，它也是
[编辑器配置](#编辑器配置)中使用的模板服务器。

## 安装依赖

```toml
[dependencies]
lspf = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

`lspf` 的 `Cargo.toml` 已经引入 `lsp-types`、`tokio`、`tracing`、`serde`
等运行时依赖，因此你的应用只需为直接使用的 `tokio` 功能选择相应 feature。

## 为什么选择 lspf

- **异步优先。** 框架端到端使用 `async fn`，没有同步处理路径。
- **最小可用服务器。** 实现 `LanguageServer` trait，并把实例交给
  `lspf::stdio(...)`，即可得到一个可工作的 LSP 服务器。
- **由框架管理文档状态。** 增量文本变更会在用户处理器运行前应用到并发安全、
  基于 rope 的 `Documents` 存储。
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
| `LanguageServer`    | 应用实现的异步 trait；当前包含生命周期和文本文档处理器。                               |
| Handler             | 处理 LSP 请求或通知的异步 trait 方法。                                                 |
| `Document`          | 框架跟踪的文本资源：URI、语言 ID、版本和基于 rope 的内容。                             |
| `Documents`         | 服务器和每个处理器 `Context` 共享的并发安全文档存储。                                 |
| `Context`           | 处理器访问 `Documents`、请求 ID、`tracing` span 和 `publish_diagnostics` 的入口。      |
| `CancellationToken` | 传递给请求处理器的取消信号。                                                           |
| `Transport`         | 供分发器使用、拆分为 reader 和 writer 两部分的消息帧通道。                             |

## 架构

完整设计文档与代码一起维护：

- [`CONTEXT.md`](./CONTEXT.md)：领域语言和共享词汇。
- [`docs/adr/`](./docs/adr/)：16 份架构决策记录，涵盖纯异步运行时、分发器设计、
  capability 自动推导、取消模型、传输形式、`Layer`/`Service` 提案、位置编码等。
  ADR 同时描述架构方向和已经交付的行为；ADR 被接受并不表示对应功能已经实现。

## 路线图

当前已经可用：

- `stdio` 和公开的自定义传输接口。
- 生命周期和增量文本文档同步。
- 并发分发、有界并发、请求取消和 `tracing` span。
- 基于 rope 的文档，以及 UTF-8/UTF-16 位置编码协商。
- `Context::publish_diagnostics`。

已有规划，但尚未承诺发布版本：

- 更多 `LanguageServer` 处理器和 capability 推导。
- 其余出站通知和请求辅助方法。
- `Layer`/`Service` 组合 API 和 panic 隔离。
- 内置 TCP、WebSocket 和 WASM worker 传输。

## 示例

可以直接在 workspace 中运行模板服务器，也可以让任何 LSP 客户端启动该进程：

```bash
cargo run -p lspf-hello
```

若要连接真实编辑器，请参阅[编辑器配置](#编辑器配置)。

## 编辑器配置

本仓库是包含两个成员的 Cargo workspace：

- [`crates/lspf`](./crates/lspf)：应用依赖的框架库（`lspf = "0.1"`）。
- [`crates/lspf-hello`](./crates/lspf-hello)：可安装的**模板服务器**。它生成通过
  stdio 使用 LSP 的 `lspf-hello` 二进制；每次收到 `textDocument/didOpen` 时，
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
