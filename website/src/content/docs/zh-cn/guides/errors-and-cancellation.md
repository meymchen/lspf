---
title: 处理错误与取消
description: 把错误、取消、截止时间和阻塞工作交给正确的所有者。
---

lspf 在拥有失败的边界报告失败。错误注册产生 `BuildError`；处理器选择要响应的 `LspError`；类型化对端操作返回 `ClientError`；服务连接则返回 `Outcome`，或者返回阻止连接完成的终止性 `Error`。区分这些路径后，应用才能决定哪些失败应发送给对端、写入日志或指标，或者交给进程监管器。

## 选择正确的错误类型

| 边界 | 类型 | 调用方的处理方式 |
| --- | --- | --- |
| `ServerBuilder::build` 或 `ClientBuilder::build` | `BuildError` | 在提供服务前修复静态注册或资源策略错误。 |
| 请求或 Command 处理器 | `LspError` | 返回对端应该收到的 JSON-RPC／LSP 错误。 |
| `ClientHandle` 或 `ServerHandle` 操作 | `ClientError` | 处理生命周期、过载、超时、关闭、编码或远端失败。 |
| `ProgressHandle` 操作 | 通过 `ClientError::Progress` 返回的 `ProgressError` | 停止使用已经结束、取消或未知的进度令牌。 |
| `Server::serve` 或 `ClientConnection::serve` | `lspf::Error` | 把 Transport 或连接建立失败视为终止性故障。 |
| 已完成连接 | `Outcome` | 在 lspf 外决定进程退出码、重启策略或清理方式。 |
| 受监管的 stdio 进程 | `ChildError` 或 `ChildOutput` | 区分设置／监管失败与最终协议／操作系统状态。 |
| `Workspace::text_document` | `WorkspaceError` | 处理资源不可用、不受支持、无效或过大的情况。 |

`BuildError` 不会发送到协议线。`Outcome` 本身不是错误：它记录连接完成清理后的 `Exit`、`TransportClosed`、`WriterFailed` 或 `InitializeFailed`。服务器二进制可以把 `outcome.code()` 交给 `std::process::exit`；嵌入式宿主可以检查具体变体并保持宿主进程运行。

## 从处理器返回 LSP 错误

| `LspError` | 错误码 | 使用场景 |
| --- | ---: | --- |
| `InvalidParams` | -32602 | 对端参数格式错误，或者没有通过方法自身的校验。 |
| `InvalidRequest` | -32600 | 请求是有效 JSON，但在服务器当前领域状态下无效。 |
| `MethodNotFound` | -32601 | 动态路由无法服务该方法；普通的未注册方法由引擎处理。 |
| `Internal` | -32603 | 本地依赖或不变量意外失败；消息中不要包含秘密。 |
| `RequestCancelled` | -32800 | 对端取消了工作，或者处理器协作式接受了取消。 |
| `ContentModified` | -32801 | 文档已经改变，计算结果因此过期。 |
| `ServerCancelled` | -32802 | 服务器出于自身原因停止工作，客户端可以重试。 |
| `RequestFailed` | -32803 | 请求有效，但无法完成。 |
| `ServerNotInitialized` | -32002 | 请求在初始化前到达；通常由引擎负责该响应。 |
| `ServerError` | 应用定义 | 私有扩展需要稳定的自定义错误码、消息和可选数据。 |

辅助构造函数覆盖常见校验失败：

```rust
# use lspf::LspError;
fn parse_limit(raw: &str) -> Result<usize, LspError> {
    raw.parse()
        .map_err(|error| LspError::invalid_params(format!("invalid limit: {error}")))
}

fn missing_document(uri: &str) -> LspError {
    LspError::invalid_request(format!("document is not open: {uri}"))
}
```

如果 LSP 结果类型已经有空值形式，不要把预期中的“没有结果”转换为错误。例如，没有悬停内容时，hover 处理器通常返回 `Ok(None)`。

## 请求取消

每个请求和 Command 处理器都会收到请求作用域的 `CancellationToken`。对端发送 `$/cancelRequest`、连接成功关闭或者处理器截止时间到期时，该令牌都会取消。引擎的完成门只选择一个响应，因此迟到的处理器结果不会与取消竞争并产生第二个响应。

等待异步 I/O 本身就支持协作式取消。如果操作不接受令牌，可以使用 `tokio::select!`：

```rust,no_run
# use lspf::{CancellationToken, LspError};
# async fn remote_lookup() -> Result<String, std::io::Error> { Ok(String::new()) }
async fn cancellable_lookup(cancellation: CancellationToken) -> Result<String, LspError> {
    tokio::select! {
        _ = cancellation.cancelled() => Err(LspError::RequestCancelled),
        result = remote_lookup() => result.map_err(LspError::internal),
    }
}
```

CPU 工作不会因为处理器是异步函数而自动让出执行权。把它拆成有界单元，并在单元之间检查 `is_cancelled()`。原生目标和 WASM 目标上的取消都是协作式的，因此检查后要尽快返回。

处理器截止时间到期也会取消令牌，但引擎返回 `ServerCancelled` 和稳定消息 `handler deadline expired`。如果代码先观察到对端或应用取消，再由自己返回 `RequestCancelled`。

对于出站请求，丢弃等待中的 future 会移除关联项，并尝试发送一次 `$/cancelRequest`。连接的 `outbound_request_timeout` 执行同样操作，并返回 `ClientError::Timeout`。对端仍可能完成远端工作；迟到响应会被忽略，请求 ID 不会复用。

## 检测过期的文档工作

lspf 无法推断用户处理器结果依赖哪个文档。保存输入版本，并在返回结果前与当前保留快照比较：

```rust
# use lspf::{LspError, ServerContext};
# use lspf::types::Uri;
fn reject_stale(ctx: &ServerContext, uri: &Uri, started_at: Option<i32>) -> Result<(), LspError> {
    let current = ctx.documents().get(uri).and_then(|document| document.version());
    if current != started_at {
        return Err(LspError::ContentModified);
    }
    Ok(())
}
```

请在高成本工作结束后、构造最终响应前比较。由 provider 加载的快照，其 `version()` 为 `None`；此时应用自己的内容哈希或 generation 更适合作为过期工作键。

## 把阻塞工作移出执行器

文件系统库、解析器或会阻塞线程的原生 API 应通过 `tokio::task::spawn_blocking` 运行。阻塞闭包开始后，丢弃 join handle 不会停止它。把取消令牌的副本传进闭包，并在有界工作单元之间检查：

```rust,no_run
# use lspf::{CancellationToken, LspError};
async fn analyze(cancellation: CancellationToken) -> Result<usize, LspError> {
    let worker_cancellation = cancellation.clone();
    let worker = tokio::task::spawn_blocking(move || {
        let mut completed = 0;
        for _chunk in 0..100 {
            if worker_cancellation.is_cancelled() {
                return None;
            }
            // Run one bounded unit of blocking analysis here.
            completed += 1;
        }
        Some(completed)
    });

    match worker.await.map_err(LspError::internal)? {
        Some(completed) => Ok(completed),
        None => Err(LspError::RequestCancelled),
    }
}
```

如果运行时的共享阻塞池对当前负载过于宽松，请限制阻塞池，或者在高成本任务外加 semaphore。lspf 的入站预算限制的是已接纳协议请求，不是应用在处理器中创建的线程或子进程。

可运行的 [`blocking_work` 示例](https://github.com/meymchen/lspf/blob/main/crates/lspf/examples/blocking_work.rs)展示了阻塞工作与无关 completion 请求同时运行的情况。

## 在不暴露载荷的情况下观测失败

`ServerBuilder::on_error` 在用户 Layer 链之外接收连接失败类别和非敏感身份信息。它覆盖分帧、协议、Transport、panic 隔离、过载和关闭失败。报告不包含参数、结果、文档文本、协议线数据、panic 载荷或底层错误消息。

这个钩子适合计数器和告警。如果处理器拥有具体失败，并能正确脱敏，请使用普通应用日志。钩子中的 panic 会被隔离，不会改变清理流程或最终选中的 `Outcome`。
