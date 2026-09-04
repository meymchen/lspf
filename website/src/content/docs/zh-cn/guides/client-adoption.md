---
title: 构建 LSP 客户端
description: 通过自定义传输或受监督的 stdio 子进程连接语言服务器。
---

[`Client`](lspf::Client) 端点让应用能够连接语言服务器。它既能使用调用方提供的 [`Transport`](lspf::Transport)，也能启动并监督原生 stdio 子进程。两种方式都由 lspf 管理 LSP 请求关联和生命周期状态；工作区模型、UI、文件系统访问、进度展示，以及如何处理诊断或编辑等编辑器行为仍由应用负责。

下面提供两套完整流程。示例会作为 doctest 针对公开 crate 编译；[`public_conformance.rs`](https://github.com/meymchen/lspf/blob/main/crates/lspf/tests/public_conformance.rs) 中仅使用下游公开 API 的流程，还会让两种连接方式与真实协议对端通信。

## 选择连接方式

应用已经拥有消息通道，或需要自行控制进程和网络生命周期时，使用自定义 Transport。一个 Client 需要从启动到回收全程拥有一个原生语言服务器进程时，使用 `ClientBuilder::spawn`。

两种方式的所有权边界如下：

| 类型 | 拥有的内容 | 能否克隆 |
| --- | --- | --- |
| `Client<T>` | 初始化输入、反向处理器注册、策略，以及连接前的一个 Transport | 不能 |
| `ClientConnection` | 已初始化的通用连接及其入站协议驱动器 | 不能；`serve` 会消费它 |
| `ServerHandle` | 该连接上从客户端到服务器的类型化调用和生命周期转换 | 能 |
| `ClientContext` | 一次反向调用的请求 ID、追踪 span 和一个 `ServerHandle` | 能 |
| `ChildConnection` | 一个 `ClientConnection`、子进程、协议驱动器和 stderr 排空任务 | 不能；`shutdown` 或 `wait` 会消费它 |

`ServerHandle` 刻意比两种连接所有者都小。需要发送请求或通知的任务可以克隆它，但只能由一个任务负责最终生命周期。

## 流程一：通过自定义 Transport 连接

这个示例把两条 Tokio 通道作为已经完成消息分帧的 Transport。为了让示例能够独立运行，其中一端启动了 Server；真实宿主应把这一端替换成已有的通道适配器。

```rust,no_run
use std::time::Duration;

use lspf::types::ClientCapabilities;
use lspf::types::request::Request;
use lspf::{
    Client, LspError, Outcome, RawMessage, ResourcePolicy, Server, ServerContext,
    Transport, TransportError, TransportReader, TransportWriter,
};
use tokio::sync::mpsc;

enum Echo {}

impl Request for Echo {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "example/echo";
}

enum Confirm {}

impl Request for Confirm {
    type Params = String;
    type Result = bool;
    const METHOD: &'static str = "example/confirm";
}

type Incoming = Result<RawMessage, TransportError>;

struct ChannelTransport {
    incoming: mpsc::UnboundedReceiver<Incoming>,
    outgoing: mpsc::UnboundedSender<Incoming>,
}

struct ChannelReader(mpsc::UnboundedReceiver<Incoming>);
struct ChannelWriter(mpsc::UnboundedSender<Incoming>);

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            ChannelReader(self.incoming),
            ChannelWriter(self.outgoing),
        )
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Incoming {
        self.0.recv().await.unwrap_or(Err(TransportError::Closed))
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        self.0
            .send(Ok(message))
            .map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

fn channel_pair() -> (ChannelTransport, ChannelTransport) {
    let (to_server, server_incoming) = mpsc::unbounded_channel();
    let (to_client, client_incoming) = mpsc::unbounded_channel();
    (
        ChannelTransport {
            incoming: server_incoming,
            outgoing: to_client,
        },
        ChannelTransport {
            incoming: client_incoming,
            outgoing: to_server,
        },
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (server_transport, client_transport) = channel_pair();

    let language_server = Server::builder(())
        .request::<Echo, _, _>(
            |_state, ctx: ServerContext, text, _cancellation| async move {
                // A Server handler can make a typed reverse request.
                let accepted = ctx
                    .client()
                    .request::<Confirm>(text.clone())
                    .await
                    .map_err(LspError::internal)?;
                Ok(if accepted { text } else { String::new() })
            },
        )
        .build()?;
    let server_task = tokio::spawn(language_server.serve(server_transport));

    let mut policy = ResourcePolicy::default();
    policy.max_inbound_requests = 32;
    policy.max_outbound_messages = 256;
    policy.max_outbound_bytes = 4 * 1024 * 1024;
    policy.outbound_request_timeout = Some(Duration::from_secs(5));
    policy.handler_timeout = Duration::from_secs(10);

    let client = Client::builder(ClientCapabilities::default())
        .resource_policy(policy)
        .request::<Confirm, _, _>(|ctx, text, cancellation| async move {
            // ClientContext contains protocol state, not an editor model. A
            // nested request can use ctx.server(); UI policy belongs outside.
            let _server = ctx.server();
            tokio::select! {
                _ = cancellation.cancelled() => Err(LspError::RequestCancelled),
                accepted = async move { Ok(!text.is_empty()) } => accepted,
            }
        })
        .build(client_transport)?;

    let connection = client.connect().await?;
    let server = connection.server();
    let client_task = tokio::spawn(connection.serve());

    assert_eq!(server.request::<Echo>("hello".into()).await?, "hello");

    // One task owns the orderly terminal sequence. After shutdown succeeds,
    // only exit or disconnect is valid.
    server.shutdown().await?;
    server.exit()?;
    assert_eq!(client_task.await??, Outcome::Exit { code: 0 });
    assert_eq!(server_task.await??, Outcome::Exit { code: 0 });
    Ok(())
}
```

适配器每次读取必须产出一个完整 `RawMessage`，保持发送顺序，并在普通 EOF 或对端关闭时返回 `TransportError::Closed`。分帧或 I/O 失败会结束连接；lspf 不会在应用不知情时自动重连。`ClientConnection::serve` 返回时，待处理的类型化调用会得到结果而不会永久等待。应用需要在不发送 `shutdown` 和 `exit` 的情况下关闭本端时，调用 `ServerHandle::disconnect`。

## 流程二：拥有一个 stdio 语言服务器子进程

启用默认 `stdio` 功能后，`ClientBuilder::spawn` 会连接 stdin、stdout 和 stderr，完成初始化，启动协议驱动器和 stderr 排空任务，然后返回一个 `ChildConnection`。下面的反向通知处理器把原始诊断存入调用方状态；如何展示仍是编辑器的职责。

```rust,no_run
use std::sync::Arc;
use std::time::Duration;

use lspf::types::notification::{DidOpenTextDocument, PublishDiagnostics};
use lspf::types::{
    ClientCapabilities, DidOpenTextDocumentParams, PublishDiagnosticsParams,
    TextDocumentItem, Uri,
};
use lspf::{Client, ResourcePolicy};
use tokio::process::Command;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let diagnostics = Arc::new(Mutex::new(Vec::<PublishDiagnosticsParams>::new()));
    let diagnostics_for_handler = Arc::clone(&diagnostics);

    let mut policy = ResourcePolicy::default();
    policy.outbound_request_timeout = Some(Duration::from_secs(10));

    let child = Client::builder(ClientCapabilities::default())
        .resource_policy(policy)
        .notification::<PublishDiagnostics, _, _>(move |_ctx, params| {
            let diagnostics = Arc::clone(&diagnostics_for_handler);
            async move {
                diagnostics.lock().await.push(params);
            }
        })
        .spawn(Command::new("rust-analyzer"))
        .await?;

    let server = child.server();
    server.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: "file:///workspace/src/main.rs".parse::<Uri>()?,
            language_id: "rust".into(),
            version: 1,
            text: "fn main() {}\n".into(),
        },
    })?;

    // This consumes the owner, sends shutdown and exit, and reaps the process.
    let output = child.shutdown().await?;
    if !output.stderr().is_empty() {
        eprintln!("{}", String::from_utf8_lossy(output.stderr()));
    }
    if output.stderr_truncated() {
        eprintln!("language-server stderr was truncated");
    }
    if !output.status().success() {
        return Err(format!("language server exited with {}", output.status()).into());
    }
    Ok(())
}
```

应用主动优雅停止时使用 `shutdown`；预计子进程自行退出时使用 `wait`。后者返回相同的 `ChildOutput`，其中包含协议 `Outcome`、操作系统状态和捕获的 stderr。进程提前退出会关闭连接，并让待处理请求以 `ClientError::Cancelled` 结束。决定重启或报告失败前，应检查状态和 stderr。

stderr 始终会被排空，以免管道死锁。`ChildOutput` 保留最初 64 KiB，并记录其余内容是否被截断。优雅关闭卡住时，监督逻辑会在逐级 terminate 和 kill 前进行有界等待。丢弃仍存活的 `ChildConnection` 也会把进程交给清理代码回收，但显式调用 `shutdown` 或 `wait` 更好，因为应用能取得终止证据。

## 截止时间与取消

`ResourcePolicy` 属于一条 Client 连接。`max_inbound_requests` 限制处理器开始工作前已接纳的反向请求；`max_outbound_messages` 和 `max_outbound_bytes` 限制排队中的客户端到服务器流量。普通 `ServerHandle` 发送若无法进入队列，会返回 `ClientError::OutboundOverloaded`，且不会保留消息或待处理请求。三个限制都必须大于零。

`outbound_request_timeout` 适用于通过 `ServerHandle` 发送的请求；到期会返回 `ClientError::Timeout`、移除待处理请求，并尝试发送一次 `$/cancelRequest`。只有应用另有明确截止时间时，才把它设为 `None`。

`handler_timeout` 限制反向请求处理器。对端取消或截止时间到期会触发处理器的 `CancellationToken`；处理器应立即停止工作，并在适当时返回 `LspError::RequestCancelled`。丢弃待处理请求的 future 也会移除它并尝试取消。请求 ID 永不复用，因此迟到的响应不能满足后来的请求。

## 故障与关闭清单

- 使用 `ServerHandle` 的同时，必须并发驱动自定义 `ClientConnection::serve`。
- 只让一个任务负责依次执行 `shutdown` 和 `exit`。不需要优雅协议流量的本地拆除使用 `disconnect`。
- Transport 失败、EOF 和子进程提前退出都应视为终止事件。共享关闭路径会解决待处理调用。
- 编辑器状态和策略应放在应用拥有、由反向处理器捕获的值中；`ClientContext` 只含协议状态。
- 优先使用 `ChildConnection::shutdown` 或 `wait`，不要依赖 Drop，这样应用才能检查 `Outcome`、退出状态和 stderr。
