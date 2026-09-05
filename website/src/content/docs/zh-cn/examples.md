---
title: 功能示例服务器
description: 运行一组小型语言服务器，分别观察不同 LSP 功能的实现。
---

每个示例都是可以运行的 stdio 语言服务器。解析器刻意保持简单，方便你把注意力放在协议交互上。

## 运行示例

```console
cargo run -p lspf --example hover
```

进程会等待来自标准输入的 LSP 客户端。在本仓库的 VS Code 中运行 `Run LSP example client (select example)`，可以启动连接到所选示例的 Extension Development Host。

所有示例都把日志写入 stderr，因为 stdout 承载 LSP 线路协议。`RUST_LOG` 用于选择事件；`RUST_LOG=lspf=trace` 会启用完整框架跟踪。`LSPF_LOG_FORMAT=json` 会让每行包含一个机器可读事件，其他值则输出纯文本。

## 语言功能

| 示例 | 演示内容 |
| --- | --- |
| [`code_actions`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/code_actions.rs) | 代码操作 |
| [`code_lens`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/code_lens.rs) | Code Lens、resolve、命令和工作区编辑 |
| [`colors`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/colors.rs) | 文档颜色与颜色表示 |
| [`formatting`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/formatting.rs) | 文档、范围和输入时格式化 |
| [`goto`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/goto.rs) | 声明、定义、实现、类型定义和引用跳转 |
| [`hover`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/hover.rs) | 基于同步文档文本生成悬停信息 |
| [`inlay_hints`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/inlay_hints.rs) | Inlay Hint 与 resolve |
| [`links`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/links.rs) | 文档链接与 resolve |
| [`publish_diagnostics`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/publish_diagnostics.rs) | 从文档钩子推送诊断 |
| [`pull_diagnostics`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/pull_diagnostics.rs) | 文档与工作区拉取式诊断 |
| [`rename`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/rename.rs) | 重命名准备与重命名 |
| [`semantic_tokens`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/semantic_tokens.rs) | 完整、增量和范围语义令牌 |
| [`symbols`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/symbols.rs) | 文档与工作区符号 |

## 框架行为

| 示例 | 演示内容 |
| --- | --- |
| [`server_features`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/server_features.rs) | 命令、进度、配置、异步工作和动态注册 |
| [`blocking_work`](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/blocking_work.rs) | 避免让阻塞工作占用异步执行器线程 |

## 传输示例

不同传输示例复用相同的处理器，因此应用逻辑不随宿主改变：

```console
cargo check -p lspf --example native_tcp --no-default-features --features tcp
cargo check -p lspf --example native_websocket --no-default-features --features websocket
```

在 VS Code 中运行 `Run LSP example client over a socket (select transport)` 并选择 `tcp` 或 `websocket`，即可启动示例，并让编辑器自己的语言客户端连接到它。Zed 通过 stdio 启动语言服务器，无法连接这些套接字示例。

如需无人值守检查两个适配器，请运行：

```console
node tools/lsp-transport-probe/main.mjs both
```

[传输指南](guides/transports)介绍原生套接字以及浏览器或 Node Worker，其中包含构建和宿主命令。
