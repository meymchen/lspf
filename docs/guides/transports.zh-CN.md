# 选择与实现 Transport

[English](./transports.md) | [简体中文](./transports.zh-CN.md)

一个 `Server` 持有一个 LSP 连接。Transport 是传递该连接 JSON-RPC envelope 的消息帧
通道。应根据连接所在的 host 选择 Transport；处理器注册和业务逻辑不随 Transport
改变。

## 选择指南

| Host 与连接 | 选择 | Cargo 参数 | 协议 framing |
| --- | --- | --- | --- |
| 编辑器启动原生进程 | stdio | 默认 feature，或 `--no-default-features --features stdio` | `Content-Length` |
| 一个原生客户端连接端口 | TCP | `--no-default-features --features tcp` | `Content-Length` |
| 一个原生 WebSocket 客户端连接 | WebSocket | `--no-default-features --features websocket` | 每个 text 或 binary message 包含一个 JSON envelope |
| Browser 或 Node host 把 port 传给 WASM Worker | worker-channel | `--target wasm32-unknown-unknown --no-default-features --features worker-channel` | 每个 `MessagePort` message 包含一个 JSON envelope |
| 嵌入环境已有其他消息通道 | 自定义 Transport | 原生环境启用 `runtime-tokio`，WASM 环境启用 `wasm`，再添加 adapter 所需依赖 | 由 adapter 定义 |

内置 TCP 和 WebSocket builder 只 bind 一次，接受一个客户端后便丢弃 listener。如需
服务另一个连接，请新建 `Server`；连接状态不会共享。

## Cargo feature

默认 feature 只选择 `stdio`。使用其他 Transport 且不希望引入 stdio 依赖树时，设置
`default-features = false`。

```toml
[dependencies]
lspf = { version = "0.5.2", default-features = false, features = ["tcp"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

该 crate 要求 Rust 1.98 或更高版本。上例只启用 TCP adapter；使用 stdio 时保留默认
feature 即可。

| Feature | 默认启用 | 启用内容 | 公开效果 |
| --- | --- | --- | --- |
| `default` | 是 | `stdio` | 原生 stdio 使用方式 |
| `stdio` | 通过 `default` | `runtime-tokio`、`tokio-util/codec`、`tokio/process` | 在原生 target 提供 `stdio`、`StdioBuilder`、stdio Transport 类型与 stdio 子进程监管 |
| `tcp` | 否 | `runtime-tokio`、`tokio-util/codec`、`tokio/net` | 在原生 target 提供 `tcp`、`TcpBuilder` 和 TCP Transport 类型 |
| `websocket` | 否 | `runtime-tokio`、`tokio-tungstenite`、`tokio/net` | 在原生 target 提供 `websocket`、`WebSocketBuilder` 和 WebSocket Transport 类型 |
| `runtime-tokio` | 通过原生 Transport | `tokio` | 为 `Server::serve` 提供原生执行环境，本身不提供 I/O adapter |
| `wasm` | 否 | `wasm-bindgen-futures` | 为 `Server::serve` 提供 WASM 执行环境，本身不提供 I/O adapter |
| `worker-channel` | 否 | `wasm`、`js-sys`、`wasm-bindgen`、`web-sys` | 在 `wasm32` 提供 `worker_channel` 和 `MessagePort` Transport 类型 |
| `proposed` | 否 | 无其他内容 | 增加 draft LSP 类型与 client 辅助方法，不依赖任何 Transport |

应用还要直接列出自己使用的依赖。例如，即使 lspf 选择的原生 Transport 内部使用
Tokio，带 `#[tokio::main]` 的二进制仍然需要自己的 `tokio` 依赖。

## Target 与 feature 兼容性

| Target 与 feature 组合 | 状态 | 原因或可用运行方式 |
| --- | --- | --- |
| 原生默认 feature 或 `stdio` | 支持 | 使用 `lspf::stdio(server)` |
| 原生 `tcp` | 支持 | 使用 `lspf::tcp(server, address)` |
| 原生 `websocket` | 支持 | 使用 `lspf::websocket(server, address)` |
| 原生 `runtime-tokio`，无 adapter | 支持自定义 Transport | 调用 `server.serve(custom_transport)` |
| 原生环境，无 runtime feature | 支持仅协议编译 | 注册与协议类型可用，但不能运行服务器 |
| 原生 `worker-channel` | 明确无效 | `MessagePort` 属于 WASM Worker，该 feature 会触发编译错误 |
| `wasm32-unknown-unknown` 与 `worker-channel` | 支持 | 该 feature 隐含 `wasm`；使用 `lspf::worker_channel(server, port)` |
| `wasm32-unknown-unknown` 与 `wasm`，无 adapter | 支持自定义 Transport | 调用 `server.serve(custom_transport)` |
| `wasm32-unknown-unknown`，无 `wasm` | 明确无效 | 每个 WASM build 都需要对应 runtime glue |
| `wasm32-unknown-unknown` 默认 feature 或 `stdio` | 不支持 | stdio 是原生 adapter，需要关闭默认 feature |
| `wasm32-unknown-unknown` 与 `tcp` 或 `websocket` | 明确无效 | 这些 adapter 依赖原生 Tokio socket，并会触发编译错误 |
| 任意受支持组合加 `proposed` | 支持 | `proposed` 只增加协议 API，不选择 Transport 或 runtime |

不要在同一次 build 中组合原生 adapter 与 `worker-channel`。同时发布原生和 WASM
artifact 的项目应在不同 Cargo 命令中选择各自的 feature。

## 可构建示例与共享处理器

所有示例处理器都位于
[`examples/shared/mod.rs`](../../crates/lspf/examples/shared/mod.rs)。每个 host 注册的
`hover`、`completion`、`shared/ping` 与 `didOpen` 钩子名称、参数和返回类型相同，
只有最后的运行调用不同：

- [`shared_server.rs`](../../crates/lspf/examples/shared_server.rs) 通过 stdio 运行共享
  处理器，同时可作为只有 runtime 的 WASM 示例编译。
- [`native_tcp.rs`](../../crates/lspf/examples/native_tcp.rs) 服务一个 TCP 连接。
- [`native_websocket.rs`](../../crates/lspf/examples/native_websocket.rs) 服务一个
  WebSocket 连接。
- [`worker_channel.rs`](../../crates/lspf/examples/worker_channel.rs) 为 browser 与 Node
  Worker 导出 wasm-bindgen `serve(MessagePort)` 函数。配套的
  [`browser`](../../crates/lspf/examples/worker_channel_hosts/browser/package.json) 和
  [`node`](../../crates/lspf/examples/worker_channel_hosts/node/package.json) host package
  会编译 Rust export、生成对应 JavaScript glue，并检查 host 文件。

分别构建原生示例，确保每次只解析所需 adapter：

```bash
cargo check -p lspf --example native_tcp \
  --no-default-features --features tcp
cargo check -p lspf --example native_websocket \
  --no-default-features --features websocket
```

为真实 target 构建两个 WASM 示例：

```bash
cargo check -p lspf --example shared_server \
  --target wasm32-unknown-unknown --no-default-features --features wasm
cargo check -p lspf --example worker_channel \
  --target wasm32-unknown-unknown --no-default-features \
  --features worker-channel
```

### Browser Worker host

安装与 `Cargo.lock` 匹配的 wasm-bindgen CLI，再从仓库根目录构建 browser host：

```bash
cargo install wasm-bindgen-cli --version 0.2.126 --locked
npm --prefix crates/lspf/examples/worker_channel_hosts/browser run build
```

该 package 会先运行以下 Rust 与 wasm-bindgen 命令，再用 Node 的 JavaScript parser
检查 host module：

```bash
cargo build -p lspf --example worker_channel \
  --target wasm32-unknown-unknown --no-default-features \
  --features worker-channel --locked
wasm-bindgen --target web \
  --out-dir crates/lspf/examples/worker_channel_hosts/browser/pkg \
  target/wasm32-unknown-unknown/debug/examples/worker_channel.wasm
```

通过 HTTP server 提供
[`browser` 目录](../../crates/lspf/examples/worker_channel_hosts/browser/index.html)，再打开
`index.html`。`main.mjs` 创建 channel 并传递其中一个 endpoint；它导出的 `lspPort`
属于 LSP client。module Worker 初始化生成的 web binding，再把传入的 port 交给 Rust
的 `serve` export。

### Node Worker host

从仓库根目录构建并运行 Node host：

```bash
npm --prefix crates/lspf/examples/worker_channel_hosts/node run build
npm --prefix crates/lspf/examples/worker_channel_hosts/node run smoke
```

build 会运行上面的同一条 Cargo 命令，并通过以下命令生成 CommonJS binding：

```bash
wasm-bindgen --target nodejs \
  --out-dir crates/lspf/examples/worker_channel_hosts/node/pkg \
  target/wasm32-unknown-unknown/debug/examples/worker_channel.wasm
```

[`main.cjs`](../../crates/lspf/examples/worker_channel_hosts/node/main.cjs) 创建
`worker_threads.MessageChannel`，把 server port 传给
[`worker.cjs`](../../crates/lspf/examples/worker_channel_hosts/node/worker.cjs)，再使用
client port 完成 initialize、initialized、shutdown 和 exit。只有 Worker 返回成功
`Outcome` 时，smoke 命令才会通过。

JavaScript host 负责创建和终止 Worker。lspf adapter 只启动和关闭传入的 port。

## Stdio 规则

stdio 只在原生 target 启用 `stdio` feature 时可用。它是 binary I/O 通道：lspf 按
LSP `Content-Length: N\r\n\r\n` framing 读取 stdin 和写入 stdout。不要向 stdout
打印人类可读内容；`tracing` 和其他日志输出都必须写入 stderr。

`lspf::stdio(server).serve().await` 返回 `Outcome`，不会终止进程。二进制自行决定是否
调用 `outcome.code()`、报告错误、重启或执行其他清理。reader EOF、客户端关闭、协议
`exit` 与初始化致命错误都通过同一 outcome 路径返回。

### 启动语言服务器子进程

原生 Client 可以独占任意 command，并将其作为一个受监管的 stdio 子进程。`spawn`
会把三个标准流设置替换为 pipe，连接并初始化 Client，驱动入站协议流量，同时持续排空
stderr：

```rust,no_run
use lspf::types::ClientCapabilities;
use lspf::Client;
use tokio::process::Command;

# async fn run() -> Result<(), lspf::ChildError> {
let command = Command::new("rust-analyzer");
let child = Client::builder(ClientCapabilities::default())
    .spawn(command)
    .await?;
let server = child.server();

// 子进程存活期间，通过 `server` 发送带类型的请求与通知。
let output = child.shutdown().await?;
assert!(output.status().success());
# Ok(())
# }
```

`shutdown` 依次发送 LSP `shutdown` 请求与 `exit` 通知，然后回收进程。若子进程不退出，
清理会依次执行有界等待、terminate、再次有界等待与 kill。`wait` 则用于观察自行退出的
服务器。两者都会返回协议 `Outcome`、操作系统退出状态与 stderr 的前 64 KiB；达到
捕获上限后仍会继续排空 stderr。丢弃仍存活的 `ChildConnection` 会把资源转交给 reaper
thread，并在当前 Tokio runtime 上安排 graceful 协议清理。即使 runtime 停止，该 thread
的同步 terminate-kill-reap 路径仍会继续运行。若 Drop 执行时没有当前 runtime，则会同步
执行相同的进程清理。

## 实现自定义 Transport

实现 `Transport` 时需要提供两个独立持有所有权的 half：

- `TransportReader::recv` 每次只返回一个完整且已解码的 `RawMessage`，不能暴露部分
  byte，也不能合并 envelope。
- `TransportWriter::send` 每次只编码一个 `RawMessage`。调用都来自同一个 writer
  task，必须保持调用顺序。
- `Transport::split` 把两个 half 分别交给协议引擎 task，因此读写可以并发进行。
- `TransportWriter::shutdown` 会消费 writer。它应 flush 已接受的输出，在协议支持时
  发送 close，并释放底层 channel。

adapter 负责 wire framing 与 JSON-RPC envelope 转换。byte-stream adapter 通常添加
或移除 `Content-Length`；已经按消息分帧的 channel 则把一个 channel message 映射为
一个 `RawMessage`。分配或发送大型 body 前，应先执行有限的消息大小检查。

两个方向都必须保持顺序。不要为每次 send 新建 task，不要让 response 越过
notification，也不要递交 close 后收到的消息。普通 EOF、客户端关闭或向已关闭连接
写入时返回 `TransportError::Closed`；framing 或 envelope 数据无效时使用
`Malformed`；超过大小限制时使用 `OversizedMessage`；I/O source 有意义时使用 `Io`；
JSON 转换失败时使用 `Serde`。引擎只采用第一个观察到的关闭原因，自定义 adapter
不应在该边界后隐式重试或重连。

构造 adapter 后调用 `server.serve(custom_transport)`。底层 wrapper 由调用者提供。
例如，TLS certificate 策略、mTLS、ALPN 和 rotation 属于应用：先接受并认证 TLS
stream，再在其上实现消息帧 Transport。lspf 不会隐式添加 TLS。

## Transport 范围

内置 Transport 不提供 TLS 配置、多客户端服务、WebSocket client mode、重连、CLI
Transport 选择、notebook/client framework 或 shared-memory WASM。这些属于部署或
client framework 策略，不应隐藏在单个 `Server` 连接中。请在 lspf 外实现，或在
消息帧契约足够时提供自定义 Transport。
