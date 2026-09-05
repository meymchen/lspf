---
title: 选择传输层
description: 根据宿主环境选择 stdio、TCP、WebSocket 或 Worker Channel。
---

lspf 把协议引擎与承载消息帧的通道分开。一个 `Server` 拥有一条 LSP 连接；Transport 只承载该连接中已经分帧的 JSON-RPC 信封。传输选择不改变处理器注册或业务逻辑。

## 选择指南

| 宿主与连接 | 选择 | Cargo 功能 | 分帧方式 |
| --- | --- | --- | --- |
| 编辑器启动原生进程 | stdio | 默认或 `stdio` | `Content-Length` |
| 一个原生客户端连接端口 | TCP | `tcp` | `Content-Length` |
| 一个原生 WebSocket 客户端连接 | WebSocket | `websocket` | 每条文本或二进制消息一个 JSON 信封 |
| 浏览器或 Node 把端口转移给 WASM Worker | worker-channel | `worker-channel` | 每条 `MessagePort` 消息一个 JSON 信封 |
| 嵌入环境已有消息通道 | 自定义 Transport | 原生用 `runtime-tokio`，WASM 用 `wasm` | 由适配器定义 |

第一方 TCP 和 WebSocket 构建器只绑定一次、接收一个客户端，然后丢弃监听器。要服务更多连接，应为每个对端创建新的 `Server`；连接状态不会共享。

## Cargo 功能

```toml
[dependencies]
lspf = "1.0.0"
```

```toml
[dependencies]
lspf = { version = "1.0.0", default-features = false, features = ["tcp"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

默认功能只选择 `stdio`。`tcp`、`websocket` 和 `worker-channel` 分别加入对应适配器；`runtime-tokio` 与 `wasm` 只提供执行环境，不提供 I/O；`testing` 提供内存 Transport、脚本化对端、线上捕获、虚拟时钟和生命周期流程。应用仍需直接声明自己在代码中使用的依赖，例如 `#[tokio::main]` 所需的 `tokio`。

## 目标与功能兼容性

- 原生目标支持默认 stdio、`tcp`、`websocket`，也支持用 `runtime-tokio` 驱动自定义 Transport。
- 原生目标不选择运行时功能时，只能编译注册和协议类型，不能调用 `serve`。
- `worker-channel` 只支持 `wasm32-unknown-unknown`，并隐含 `wasm`。
- WASM 自定义 Transport 可以只选择 `wasm`；任何 WASM 构建都必须包含 WASM 运行时胶水。
- WASM 上的 stdio、TCP、WebSocket 和 `testing` 会被拒绝；原生目标上的 `worker-channel` 也会被拒绝。

原生和 WASM 产物应在不同 Cargo 命令中选择功能，不要把原生适配器与 `worker-channel` 组合进同一构建。crate 要求 Rust 1.98 或更高版本。

## 可构建示例与共享处理器

`shared_server`、`native_tcp`、`native_websocket` 和 `worker_channel` 复用同一组 hover、completion、`shared/ping` 与 `didOpen` 处理器，只有最后的服务调用不同。

```console
cargo check -p lspf --example native_tcp --no-default-features --features tcp
cargo check -p lspf --example native_websocket --no-default-features --features websocket
```

要从真实编辑器驱动任一套接字示例，请运行 VS Code 启动配置 `Run LSP example client over a socket (select transport)`。[测试客户端](https://github.com/meymchen/lspf/tree/main/tools/vscode-test-client)会启动示例，并让语言客户端连接到绑定端口，从而通过编辑器而不是脚本验证适配器。Zed 只会通过 stdio 命令启动语言服务器，不提供套接字选项，因此无法连接这两个示例。

如需无人值守检查，可运行[传输探针](https://github.com/meymchen/lspf/tree/main/tools/lsp-transport-probe)。它会为每种传输构建并启动示例，然后验证一次完整的 LSP 会话：

```bash
node tools/lsp-transport-probe/main.mjs both
```

WASM 示例使用 `--target wasm32-unknown-unknown --no-default-features`；运行时自定义 Transport 选择 `wasm`，Worker 示例选择 `worker-channel`。

### 浏览器 Worker 宿主

安装与 `Cargo.lock` 匹配的 `wasm-bindgen-cli`，再运行 `npm --prefix crates/lspf/examples/worker_channel_hosts/browser run build`。宿主创建 `MessageChannel`，把服务器端口转移给模块 Worker，并把客户端端口作为 LSP 客户端使用。Rust 适配器只启动和关闭传入端口；Worker 的创建、HTTP 托管与终止属于 JavaScript 宿主。

### Node Worker 宿主

运行 `npm --prefix crates/lspf/examples/worker_channel_hosts/node run build` 构建，再用同目录的 `run smoke` 验证。宿主用 `worker_threads.MessageChannel` 转移服务器端口，并在客户端端口完成 initialize、initialized、shutdown 和 exit。只有 Worker 返回成功 `Outcome`，smoke 命令才成功。

接下来阅读[使用 stdio 与自定义传输层](stdio-and-custom-transports)，了解进程所有权、分帧规则和嵌入式宿主。
