---
title: 报告进度与自定义消息
description: 报告长时间运行的工作、流式返回分批结果，并安全扩展协议。
---

请在基本的服务端到客户端调用流程就绪后使用这些 API。它们用于长时间运行的工作，也提供类型安全的协议扩展出口。

## 工作进度

`ClientHandle::begin_progress` 会先发送 `window/workDoneProgress/create`，再返回唯一的 `ProgressHandle`。随后用 `report` 更新消息或百分比，并且必须显式调用 `end`。取消能力只表示客户端可以请求取消；工作代码仍要检查句柄的取消令牌。丢弃句柄不会偷偷发送结束消息，而会移除令牌并记录警告。

处理已有请求时，`ServerContext::begin_progress` 使用请求自带的 `workDoneToken`。没有令牌时返回 `None`，不能擅自创建一个来冒充该请求的进度。

## 分批结果

分批结果只适用于实现 `PartialResultRequest` 且请求带有 `partialResultToken` 的方法。`ctx.partial_results::<R>()` 返回借用请求生命周期的 sink；每次 `report` 发送协议规定的分批结果类型。普通响应仍负责结束请求，没有 finish 消息，也不需要 `end`。

处理器完成后继续报告会返回 `ClientError::InvalidHelperParams`。工作进度面向用户展示，分批结果承载协议结果，两者不能互相替代。

## 自定义请求与通知

非标准方法只需定义实现 `lspf::types::request::Request` 或 `lspf::types::notification::Notification` 的标记类型，再调用通用 `request` 或 `notify`。响应按 ID 关联，可以乱序到达。远端 JSON-RPC 错误会成为包含完整 code、message 和 data 的 `ClientError::Remote`；关闭连接会以 `ClientError::Cancelled` 解决所有待处理请求。

超过出站消息或字节预算时返回 `ClientError::OutboundOverloaded`，且不会遗留待处理项。超时会返回 `ClientError::Timeout` 并尝试取消；请求 ID 永不复用，所以迟到响应不会误配给后续请求。

## 辅助方法参考

| Rust 方法 | 协议方法 | 结果 |
| --- | --- | --- |
| `publish_diagnostics` | `textDocument/publishDiagnostics` | `()` |
| `show_message`／`log_message`／`log_trace`／`telemetry_event` | 对应 window、`$/logTrace` 或 telemetry 通知 | `()` |
| `show_document` | `window/showDocument` | `ShowDocumentResult` |
| `show_message_request` | `window/showMessageRequest` | `Option<MessageActionItem>` |
| `apply_edit` | `workspace/applyEdit` | `ApplyWorkspaceEditResult` |
| `configuration` | `workspace/configuration` | `Vec<serde_json::Value>` |
| `workspace_folders` | `workspace/workspaceFolders` | `Option<Vec<WorkspaceFolder>>` |
| `register_capability`／`unregister_capability` | `client/registerCapability`／`client/unregisterCapability` | `()` |
| `refresh_*` | 对应的 `workspace/*/refresh` | `()` |
| `begin_progress` | `window/workDoneProgress/create`，随后发送 `$/progress` | `ProgressHandle` |
| `ProgressHandle::report`／`end` | `$/progress` | `()` |
| `ServerContext::partial_results` | 不直接发送；借出 sink | `Option<PartialResultSink<'_, R>>` |

通知辅助方法只表示消息已进入有界队列，不表示客户端已经处理。所有方法都可能因连接关闭、序列化失败或资源过载返回 `ClientError`，不得无条件忽略。

## 辅助方法刻意不负责的事项

- `configuration` 不提供配置缓存；`Workspace` 只由 `workspace/didChangeConfiguration` 更新。
- `publish_diagnostics` 不存储、去重或清除诊断。
- 动态注册状态、重连和撤销记录由应用维护。
- 队列过载不会隐式重试；应用决定重试、合并或跳过可选输出。
- LSP 的笔记本同步只有客户端到服务器方向，因此没有出站笔记本辅助方法。
- 丢弃 `ProgressHandle` 不会隐式发送 progress end；只有 `end` 会结束进度。
