# 功能示例服务器

[English](./README.md) | [简体中文](./README.zh-CN.md)

每个文件都是可通过 stdio 运行的语言服务器，只演示一小组 LSP 方法。parser 与语言
本身保持简单，以便直接观察协议交互。

| 示例 | 演示的方法 |
| --- | --- |
| `code_actions` | `textDocument/codeAction` |
| `code_lens` | `textDocument/codeLens`、`codeLens/resolve`、Command 与 workspace edit |
| `colors` | `textDocument/documentColor`、`textDocument/colorPresentation` |
| `formatting` | document、range 与 on-type formatting |
| `goto` | declaration、definition、implementation、type definition 与 references |
| `hover` | `textDocument/hover` |
| `inlay_hints` | `textDocument/inlayHint`、`inlayHint/resolve` |
| `links` | `textDocument/documentLink`、`documentLink/resolve` |
| `publish_diagnostics` | 在 document open 与 change 钩子中推送 diagnostics |
| `pull_diagnostics` | document 与 workspace diagnostic 请求 |
| `rename` | `textDocument/prepareRename`、`textDocument/rename` |
| `semantic_tokens` | full、full-delta 与 range semantic tokens |
| `symbols` | document 与 workspace symbol 请求 |
| `server_features` | Command、progress、configuration、异步工作与动态 client 注册 |
| `blocking_work` | 在专用 thread pool 执行阻塞工作时处理 completion |

使用 Cargo 通过 stdio 运行服务器：

```console
cargo run -p lspf --example hover
```

该命令会在 stdin 上等待 LSP client。若要在 VS Code 中交互测试，请打开仓库根目录并
运行 `Run LSP example client (select example)`。Extension Development Host 打开后，
回到仓库窗口运行 `Attach to running LSP server/example`，再选择对应示例进程。Zed 的
`.zed/debug.json` 提供相同的 attach 入口，可以调试已经由 LSP client 启动的进程。

`blocking_work` 使用 `tokio::task::spawn_blocking`，因为 lspf 处理器只支持异步模式。
`server_features` 在初始化前安装 completion 路由，再通过
`client/registerCapability` 与 `client/unregisterCapability` 控制客户端是否发送
请求。初始化后 server router 已冻结，不能再添加或移除本地路由。

## Transport 示例

Transport 示例共用 `shared/mod.rs` 中的处理器。分别构建原生 adapter，确保每次只
启用所需 feature：

```console
cargo check -p lspf --example native_tcp --no-default-features --features tcp
cargo check -p lspf --example native_websocket --no-default-features --features websocket
```

`shared_server` 在原生 target 通过 stdio 运行同一套处理器，同时检查只有 runtime 的
WASM 路径。`worker_channel` 为 browser 或 Node Worker 导出服务器。WASM build 与
host 命令见[传输指南](../../../docs/guides/transports.zh-CN.md)。
