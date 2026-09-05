---
title: 部署与排查端点
description: 选择进程拓扑、明确关闭所有权并诊断端点故障。
---

部署沿用与资源管理相同的边界：一个 `Server` 或 `Client` 拥有一条连接。本指南把这条边界应用到进程拓扑与关闭流程。

## 选择部署形态

| 部署方式 | 端点与 Transport | 运维所有者 |
| --- | --- | --- |
| 编辑器启动一个原生服务器 | `Server` 使用默认 `stdio` | 编辑器或插件负责重启进程；服务器保证 stdout 只承载协议。 |
| 服务接收原生 TCP 对端 | 每个已接收套接字使用一个 `Server` 和 `TcpTransport::from_stream` | 应用负责外层 accept 循环、身份验证、TLS 和共享状态。 |
| 服务接收 WebSocket 对端 | 每条已建立的流使用一个 `Server` 和 `WebSocketTransport::from_stream` | 应用负责 HTTP 升级策略、身份验证、TLS 和重连。 |
| 浏览器或 Node Worker | `Server` 使用 `worker-channel` | JavaScript 宿主创建并转移 `MessagePort`，随后负责终止 Worker。 |
| 应用启动语言服务器 | `ClientBuilder::spawn` | `ChildConnection` 负责驱动协议、排空 stderr、逐级终止进程并回收。 |
| 现有通道或运行时宿主 | 自定义 `Transport`，配合 `runtime-tokio` 或 `wasm` | 适配器负责分帧、大小限制、身份验证和通道生命周期。 |

第一方 TCP 和 WebSocket 构建器只接收一条连接，随后便丢弃监听器。它们不提供多租户服务器循环。第一方 Transport 也不添加 TLS、身份验证、重连或负载均衡。需要这些策略时，应先包装并验证流，再把它交给自定义 Transport。

受支持的原生宿主、WASM、Rust 版本和 Cargo 功能组合在 [`SECURITY.md`](https://github.com/meymchen/lspf/blob/main/SECURITY.md) 中完整列出。其他目标可能可以编译，但不在支持承诺内。

## 由一个所有者负责关闭

服务器应持续驱动 `serve`，直到对端发送 `exit` 或 Transport 结束。返回的 `Outcome` 会记录关闭是否有序。lspf 自身绝不会调用 `std::process::exit`。

对于使用自定义 Transport 的 Client，由一个任务驱动 `ClientConnection::serve`，同时由唯一的生命周期所有者依次调用 `ServerHandle::shutdown` 和 `exit`。若本地拆除时不希望发送优雅关闭的协议流量，请使用 `disconnect`。

对于 stdio 子进程，优先使用 `ChildConnection::shutdown` 或 `wait`。两者都会返回协议结果、操作系统状态和有大小上限的 stderr。丢弃对象仍会启动清理，但应用无法得到这些证据。

连接关闭后，会拒绝新工作、解决待处理请求、取消自有处理器任务、排空已接纳的出站消息，并等待连接任务结束。所有者结束后，不要在全局状态中继续保留连接级句柄。

## 已知限制

- lspf 是 LSP 协议框架，不是编辑器 UI、项目模型、解析器、索引、缓存或具体语言实现。
- `Client` 能分派类型化的反向流量，但不是完整的编辑器或扩展宿主框架。UI、工作区、文件系统、诊断展示和重启策略由应用负责。
- 原生执行使用 Tokio，不支持自定义原生执行器。WASM 在 `wasm32-unknown-unknown` 上面向浏览器或 Node Worker。
- 框架不内置指标导出器。请把 tracing 和 `on_error` 接入应用自行管理的指标后端。
- Transport 辅助工具只服务一条连接。多客户端接入和跨连接共享状态属于宿主应用。
- 延后实现的协议能力和未冻结的导出项记录在[冻结的公共接口](https://github.com/meymchen/lspf/blob/main/docs/public-interface.md)中，不能根据 ADR 或某个已生成的协议类型推断其存在。

## 故障排查

| 现象 | 检查项 |
| --- | --- |
| stdio 服务器在终端运行时一直等待 | 它在等待按 `Content-Length` 分帧的 LSP 输入。请使用编辑器、Client 教程或脚本化对端。 |
| 编辑器报告请求头或 JSON 格式错误 | 移除 stdout 中的所有日志和横幅；应用日志应写入 stderr。 |
| `serve` 返回 `RuntimeRequired` | 在 Tokio 运行时中启动它，例如使用 `#[tokio::main]`。lspf 不会隐式启动原生运行时。 |
| 某项功能始终收不到请求 | 确认已注册功能描述符，并检查生成的 initialize capabilities。自定义原始路由不会通告标准能力。 |
| 文档钩子没有运行 | 确认已启用文档同步，而且通知通过了验证和资源接纳。笔记本单元格变更调用笔记本钩子，而不是文本文档钩子。 |
| 高负载时请求返回 `ServerCancelled` | 检查消息：容量耗尽表示入站接纳已满；`handler deadline expired` 表示请求超过截止时间。 |
| 发送返回 `OutboundOverloaded` | 可选消息超过了消息数或编码后字节数预算。合并过期输出，或根据实测队列压力调优。 |
| emoji 附近的诊断或位置发生偏移 | 使用 `ctx.documents().position_encoding()` 和视图的转换辅助方法；UTF-16 会把代理项对计为两个单元。 |
| Client 请求超时 | 确认连接驱动器正在运行、对端实现了该方法，而且出站截止时间符合实测延迟。 |
| 受监督的子进程提前退出 | 检查 `ChildOutput::outcome`、操作系统状态、stderr 和 `stderr_truncated`；待处理请求会以已取消结束。 |
| TCP 或 WebSocket 只能服务一个对端 | 这是第一方构建器的约定。请在应用自行管理的 accept 循环中创建端点。 |
| WASM 构建选择了 stdio、TCP 或 WebSocket | 禁用默认功能，并为自定义 Transport 选择 `wasm`，或为 `MessagePort` 选择 `worker-channel`。 |

确认 API 可用性时，请查阅[冻结的公共接口](https://github.com/meymchen/lspf/blob/main/docs/public-interface.md)和无警告的 [docs.rs 参考](https://docs.rs/lspf/latest/lspf/)。要复现协议故障，可以先使用[测试指南](testing)中的内存流程。
