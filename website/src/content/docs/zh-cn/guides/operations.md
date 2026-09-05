---
title: 配置资源与可观测性策略
description: 限制连接资源，并输出有用且不含载荷的遥测。
---

一个 `Server` 或 `Client` 对应一条 LSP 连接。它的 `ResourcePolicy`、队列、截止时间和追踪标识都随连接结束。服务器的 `Documents`、`Notebooks` 和 `Workspace` 同样限定在单条连接内。一个进程要服务多个对端时，应为每个对端创建一个端点，并在 lspf 之外管理共享缓存或索引。

本指南介绍生产环境的资源预算、可观测性、部署、关闭和故障排查。权威的支持矩阵和维护周期见仓库中的 [`SECURITY.md`](https://github.com/meymchen/lspf/blob/main/SECURITY.md)。

## 从有限的默认值开始

`ResourcePolicy::default()` 为每条连接设置以下限制：

| 字段 | 默认值 | 资源占用持续到 |
| --- | ---: | --- |
| `max_inbound_requests` | 64 | 已接纳请求的某一条完成路径胜出。 |
| `max_outbound_messages` | 1,024 | 已接收消息由 Transport 发送完成或失败。 |
| `max_outbound_bytes` | 16 MiB | 出站队列中的 JSON-RPC 信封正文完成发送。 |
| `max_documents` | 1,024 | 打开的文本文档关闭；其中包括笔记本单元格。 |
| `max_document_bytes` | 64 MiB | 这些文档中保留的文本被释放。 |
| `max_notebooks` | 256 | 笔记本级元数据被释放；空笔记本也计入。 |
| `outbound_request_timeout` | 30 秒 | 发给对端的每个类型化请求结束。 |
| `handler_timeout` | 30 秒 | 每个已接纳的入站请求处理器结束。 |

所有数值限制和已启用的截止时间都必须大于零。无效策略会在任何 I/O 开始之前产生 `BuildError`。只有 `outbound_request_timeout` 可以设为 `None`；只有在其他所有者能提供明确截止时间时才应禁用它。

集中安装完整策略，不要把互不相关的选项分散到多处：

```rust
use std::time::Duration;

use lspf::{ResourcePolicy, Server};

# struct State;
# fn main() {
let policy = ResourcePolicy {
    max_inbound_requests: 32,
    max_outbound_messages: 256,
    max_outbound_bytes: 4 * 1024 * 1024,
    max_documents: 2_000,
    max_document_bytes: 128 * 1024 * 1024,
    max_notebooks: 128,
    outbound_request_timeout: Some(Duration::from_secs(10)),
    handler_timeout: Duration::from_secs(20),
};

let server = Server::builder(State)
    .resource_policy(policy)
    .build()
    .expect("the production resource policy is valid");
# let _ = server;
# }
```

`ServerBuilder::concurrency_limit` 仍是 `max_inbound_requests` 的简写；不要在不同配置路径中同时设置两者。

## 根据保留成本和延迟调优

提高限制前，先测量实际文档大小、编辑器并发请求数、响应大小和慢速对端的行为。默认值是安全边界，不是吞吐量目标。

- 如果突发请求因入站容量不足而被拒绝，先检查处理器延迟和取消行为。提高限制会让更多工作和内存同时存活。
- 如果可选通知遇到 `ClientError::OutboundOverloaded`，先合并或丢弃应用中已经过时的更新，再考虑扩大队列。
- 如果必要响应无法进入队列，连接会以 `Outcome::WriterFailed` 关闭；端点绝不会静默丢弃必要流量。
- 如果文档接纳失败，内置功能会保留之前的快照，并跳过变更后的钩子。笔记本单元格共用相同的文档预算。
- 如果有效工作经常超过截止时间，先区分队列等待、外部 I/O、阻塞型 CPU 工作和真正缓慢的对端，再决定新值。

仓库的请求工作负载测量见[性能基线](https://github.com/meymchen/lspf/blob/main/docs/performance-baselines.md)，有界内存压力流程见[浸泡测试流程](https://github.com/meymchen/lspf/blob/main/docs/soak-journeys.md)。它们是参考工作负载，不是生产环境容量承诺。

## 限制阻塞工作的规模

所有处理器都是异步的。把阻塞库移到 `spawn_blocking`，在工作单元之间检查请求的 `CancellationToken`，并用应用自行管理的限制约束昂贵任务。入站请求预算可以防止协议层无限接纳请求，但不会限制运行时的阻塞线程池、子进程、数据库连接，也不会限制应用状态中持有的内存。

[错误与取消指南](errors-and-cancellation)提供了可感知取消的阻塞工作示例。

## 输出有用且不含载荷的遥测

在 `lspf=trace` 级别，框架会发出稳定的 `rpc message`、`resource budget changed`、`deadline changed`、`request completed` 和 `connection closed` 事件：

| `message` | 字段 |
| --- | --- |
| `rpc message` | `connection_id`、`direction`、`kind`，以及存在时的 `method` 和 `request_id` |
| `resource budget changed` | `connection_id`、`resource`、`resource_action`、`resource_current`；有界资源还包含 `resource_limit`，字节预算包含 `resource_bytes` 和 `resource_bytes_limit`，`pending_requests` 还包含 `direction`、`kind`、`method`、`request_id` 和可选的 `deadline_ms` |
| `deadline changed` | `connection_id`、`direction`、`kind`、`method`、`request_id`、`deadline`、`deadline_action`、`deadline_ms`、`deadline_elapsed_ms` |
| `request completed` | `connection_id`、`direction`、`kind`、`method`、`request_id`、`latency_ms`、`completion` |
| `connection closed` | `connection_id`、`close_cause` |

`direction` 是 `inbound` 或 `outbound`。资源名称包括 `inbound_requests`、`outbound_queue`、`documents`、`notebooks` 和 `pending_requests`；资源动作包括 `admit`、`release`、`update`、`reject` 和 `rollback`。截止时间名称是 `handler` 或 `outbound_request`，动作包括 `armed`、`completed`、`cancelled` 和 `expired`。完成结果包括 `success`、`error`、`cancelled`、`deadline_expired`、`rejected` 和 `connection_closed`。关闭原因包括 `exit`、`reader_eof`、`reader_failed`、`writer_failed` 和 `initialize_failed`。可选字段不存在时会被省略，而不是写入占位值。

请求与通知 span 携带相同的连接及调用标识。为保持兼容，请求 span 还保留调试格式的 `id` 字段。

stdio 服务器的日志必须写到 stderr。stdout 是 LSP 字节流；其中任何一行人类可读文本都会破坏消息分帧。使用进程监督器时，最好输出结构化 stderr，并在协议载荷之外附加自己的构建或实例标识。

需要不经日志解析就采集指标时，注册 `ServerBuilder::on_error`。该钩子提供一个 `ConnectionFailureCategory`，以及已脱敏的连接和调用标识。它有意省略参数、结果、文档内容、线上字节、panic 载荷和底层错误文本。

```rust
# struct State;
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

数字请求 ID 会保留原值。对端控制的字符串 ID 只公开 `ConnectionRequestId::String` 变体，具体内容会被脱敏。仅当方法名由框架拥有、已经注册，或由类型化出站请求声明时，才会包含方法名；其他由对端控制的方法名会被省略。钩子中的 panic 会被捕获并记录，不会中断清理。

默认追踪事件遵循同样的载荷规则。应用事件仍可能泄漏源文本、路径、请求参数或远端消息，因此把日志发送到共享后端前，应审查这些字段。

接下来阅读[部署与排查端点](deployment-and-troubleshooting)，了解进程拓扑、关闭、限制与故障诊断。
