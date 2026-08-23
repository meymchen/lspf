# 出站 client 辅助方法

[English](./outgoing-client.md) | [简体中文](./outgoing-client.zh-CN.md)

本指南介绍连接中从服务器到客户端的一侧：处理器通过 [`Context`] 获得的带类型
[`Client`] 句柄、覆盖各项标准出站通知与请求的具名辅助方法、动态 capability
注册、work-done progress，以及自定义方法的扩展入口。这里不需要手写 JSON。每个
辅助方法都接收原生 `lsp-types` 参数并返回原生结果，各协议结构由
`crates/lspf/tests/fixtures` 中的 fixture 固定。所有示例都会作为 doctest 编译。
配置查询、workspace edit、自定义出站请求、动态注册、稳定 refresh 和可取消进度的
完整流程也在 [`crates/lspf-hello`](../../crates/lspf-hello/src/main.rs) 中真实运行，
并由相邻的 stdio 端到端测试验证。

## 通知

通知发送后不等待回应：框架同步编码并放入队列，不分配 ID。只有连接正在关闭或参数
无法编码时才会失败。稳定出站接口包括
[`publish_diagnostics`](lspf::Client::publish_diagnostics)、
[`show_message`](lspf::Client::show_message)、
[`log_message`](lspf::Client::log_message)、
[`log_trace`](lspf::Client::log_trace)、
[`telemetry_event`](lspf::Client::telemetry_event) 和
[`progress`](lspf::Client::progress)，它们都会原样发送参数：

```rust
# use lspf::types::{MessageType, ShowMessageParams};
# use lspf::{Client, ClientError};
# fn announce(client: &Client) {
// The message goes to the client only; it is never echoed into the server's
// local tracing stream.
if let Err(error) = client.show_message(ShowMessageParams {
    typ: MessageType::INFO,
    message: "reindexing finished".into(),
}) {
    tracing::warn!(%error, "show_message failed");
}
# }
```

其中两个辅助方法还有额外约定。`log_trace` 受连接 trace level 控制。在客户端发送
`$/setTrace` 前，初始 level 为 `Off`，此时该方法不入队并返回 `Ok(())`；发送日志
本身不会修改 level。`publish_diagnostics` 原样发送参数，包括调用者提供的
`version`。框架不会缓存或去重 diagnostics，关闭文档也不会自动清除它们。
[`Context::publish_diagnostics`](lspf::Context::publish_diagnostics) 只是转发给 client
辅助方法，并返回相同的 `Result<(), ClientError>`。

## Window 与 workspace 请求

每个请求都会保留一个仅供当前连接使用且永不复用的 ID，然后异步等待对应响应。
具名辅助方法覆盖标准 window 交互
[`show_document`](lspf::Client::show_document) 与
[`show_message_request`](lspf::Client::show_message_request)，以及 workspace 交互
[`apply_edit`](lspf::Client::apply_edit)、
[`configuration`](lspf::Client::configuration) 与
[`workspace_folders`](lspf::Client::workspace_folders)。它们都是同一个带类型请求代理
的轻量封装：参数和结果均原样传递，不附加 UI、消息选择或编辑策略。

配置查询会按请求顺序为每个 item 返回一个 JSON 值，没有回答的 item 为 `null`。
结果只交给调用者，不会写入框架持有的 [`Workspace`](lspf::Workspace) snapshot；后者
仍由 `workspace/didChangeConfiguration` 通知同步：

```rust
# use lspf::types::{ConfigurationItem, ConfigurationParams};
# use lspf::{Client, ClientError};
async fn tab_size(client: &Client) -> Result<Option<u64>, ClientError> {
    let values = client
        .configuration(ConfigurationParams {
            items: vec![ConfigurationItem {
                scope_uri: None,
                section: Some("editor.tabSize".into()),
            }],
        })
        .await?;
    Ok(values.first().and_then(|value| value.as_u64()))
}
```

workspace edit 会按构造内容发送，不过滤、不合并，也不改写；客户端的处理结果同样原样
返回：

```rust
# use lspf::types::{ApplyWorkspaceEditParams, Position, Range, TextEdit, Uri, WorkspaceEdit};
# use lspf::{Client, ClientError};
async fn insert_header(client: &Client, uri: Uri) -> Result<bool, ClientError> {
    let edit = WorkspaceEdit {
        changes: Some(
            [(
                uri,
                vec![TextEdit {
                    range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                    new_text: "// generated\n".into(),
                }],
            )]
            .into_iter()
            .collect(),
        ),
        ..WorkspaceEdit::default()
    };
    let response = client
        .apply_edit(ApplyWorkspaceEditParams {
            label: Some("insert header".into()),
            edit,
        })
        .await?;
    Ok(response.applied)
}
```

## 动态注册

[`register_capability`](lspf::Client::register_capability) 与
[`unregister_capability`](lspf::Client::unregister_capability) 在运行时向客户端声明
capability 变化。它们只负责声明：已经冻结的 [`Router`](lspf::Server) 与初始化时
计算出的 capability 不会改变，框架也不会另存一份已向客户端注册的 capability
清单。注册所指向的本地路由必须已经通过静态注册或初始化条件注册存在。

```rust
# use lspf::types::{Registration, RegistrationParams};
# use lspf::{Client, ClientError};
async fn watch_files(client: &Client) -> Result<(), ClientError> {
    client
        .register_capability(RegistrationParams {
            registrations: vec![Registration {
                id: "my-server.watch".into(),
                method: "workspace/didChangeWatchedFiles".into(),
                register_options: None,
            }],
        })
        .await
}
```

## Workspace refresh

五个辅助方法要求客户端重新拉取稳定 workspace 功能：
[`code_lens_refresh`](lspf::Client::code_lens_refresh)、
[`diagnostic_refresh`](lspf::Client::diagnostic_refresh)、
[`inlay_hint_refresh`](lspf::Client::inlay_hint_refresh)、
[`inline_value_refresh`](lspf::Client::inline_value_refresh) 和
[`semantic_tokens_refresh`](lspf::Client::semantic_tokens_refresh)。它们不接收参数，
并把客户端的 `null` 确认转换为 `()`。辅助方法不决定重新计算策略，框架也不保存可由
它们修改的 lens、diagnostic、hint、value 或 token 状态。

启用非默认 `proposed` Cargo feature 后，还会提供
[`refresh_folding_ranges`](lspf::Client::refresh_folding_ranges) 和
[`refresh_text_document_content`](lspf::Client::refresh_text_document_content)。因为
`lsp-types` 0.97.x 没有对应类型，它们使用 [`lspf::proposed`](lspf::proposed) 中的
request marker。该 feature 只增加 API，不改变宣告的 capability。默认接口不包含
proposed、draft 或 notebook 方法。

## Work-done progress

[`begin_progress`](lspf::Client::begin_progress) 以一个故障安全操作完成连接范围内的
work-done 生命周期：从单调序列分配连接内 numeric token，完成
`window/workDoneProgress/create`，只在客户端成功响应后注册 token，并发送一条原样
携带选项的 begin 通知。返回的 [`ProgressHandle`] 用于 report 与 end。`end` 会消费
handle，无论入队成功与否都会移除 token，因此不会泄漏生命周期。

```rust
# use lspf::{Client, ClientError, ProgressOptions};
async fn index_workspace(client: &Client) -> Result<(), ClientError> {
    let progress = client
        .begin_progress(
            ProgressOptions::new("Indexing")
                .cancellable(true)
                .message("starting")
                .percentage(0),
        )
        .await?;
    for done in 1..=4u32 {
        // The client cancels through `window/workDoneProgress/cancel`; the
        // framework fires this token and sends nothing by itself. What
        // happens next — including the final message — is the application's
        // choice.
        if progress.cancellation_token().is_cancelled() {
            break;
        }
        progress.report(None, Some(done * 25))?;
    }
    progress.end(Some("index ready".into()))?;
    Ok(())
}
```

begin 失败不会留下已注册 token。report 的百分比不在 0 到 100 之间时，会在发送前
失败。丢弃仍处于活动状态的 handle 会移除 token 并记录警告，但不执行 I/O，也不会
隐式发送 end，因此应明确调用 `end`。

## 自定义请求与通知

非标准出站方法不需要新机制。定义实现
[`lsp_types::request::Request`](lspf::types::request::Request) 或
[`lsp_types::notification::Notification`](lspf::types::notification::Notification) 的
marker type，再调用通用 [`request`](lspf::Client::request) 或
[`notify`](lspf::Client::notify)：

```rust
use lspf::types::request::Request;
use lspf::{Client, ClientError};

enum SyntaxTree {}

impl Request for SyntaxTree {
    type Params = serde_json::Value;
    type Result = String;
    const METHOD: &'static str = "rust-analyzer/syntaxTree";
}

async fn syntax_tree(client: &Client, params: serde_json::Value) -> Result<String, ClientError> {
    client.request::<SyntaxTree>(params).await
}
```

所有请求都使用相同的代理语义：响应可以任意顺序到达，并通过 ID 关联；响应到达前
丢弃 future 会移除 pending entry，并发送一次 `$/cancelRequest`；连接关闭时，所有
pending 请求都以 `ClientError::Cancelled` 完成；客户端返回的 JSON-RPC error 会
变为 [`ClientError::Remote`](lspf::ClientError::Remote)，并保留完整 code、message
和 data。

## 辅助方法参考

下表列出所有具名辅助方法、协议方法、参数与结果类型，以及稳定性。通知辅助方法和
`ProgressHandle::report`/`end` 返回 `Result<(), ClientError>`；请求辅助方法返回
`Result<R, ClientError>`，其中 `R` 如表中所示。

| Rust 方法 | 协议方法 | 参数 | 结果 | 状态 |
| --- | --- | --- | --- | --- |
| `Client::publish_diagnostics` | `textDocument/publishDiagnostics` | `PublishDiagnosticsParams` | `()` | stable |
| `Client::show_message` | `window/showMessage` | `ShowMessageParams` | `()` | stable |
| `Client::log_message` | `window/logMessage` | `LogMessageParams` | `()` | stable |
| `Client::log_trace` | `$/logTrace` | `LogTraceParams` | `()` | stable |
| `Client::telemetry_event` | `telemetry/event` | `TelemetryEventParams` | `()` | stable |
| `Client::progress` | `$/progress` | `ProgressParams` | `()` | stable |
| `Context::publish_diagnostics` | `textDocument/publishDiagnostics` | `PublishDiagnosticsParams` | `()` | stable |
| `Client::show_document` | `window/showDocument` | `ShowDocumentParams` | `ShowDocumentResult` | stable |
| `Client::show_message_request` | `window/showMessageRequest` | `ShowMessageRequestParams` | `Option<MessageActionItem>` | stable |
| `Client::apply_edit` | `workspace/applyEdit` | `ApplyWorkspaceEditParams` | `ApplyWorkspaceEditResponse` | stable |
| `Client::configuration` | `workspace/configuration` | `ConfigurationParams` | `Vec<serde_json::Value>` | stable |
| `Client::workspace_folders` | `workspace/workspaceFolders` | 无（`null`） | `Option<Vec<WorkspaceFolder>>` | stable |
| `Client::register_capability` | `client/registerCapability` | `RegistrationParams` | `()` | stable |
| `Client::unregister_capability` | `client/unregisterCapability` | `UnregistrationParams` | `()` | stable |
| `Client::code_lens_refresh` | `workspace/codeLens/refresh` | 无（`null`） | `()` | stable |
| `Client::diagnostic_refresh` | `workspace/diagnostic/refresh` | 无（`null`） | `()` | stable |
| `Client::inlay_hint_refresh` | `workspace/inlayHint/refresh` | 无（`null`） | `()` | stable |
| `Client::inline_value_refresh` | `workspace/inlineValue/refresh` | 无（`null`） | `()` | stable |
| `Client::semantic_tokens_refresh` | `workspace/semanticTokens/refresh` | 无（`null`） | `()` | stable |
| `Client::refresh_folding_ranges` | `workspace/foldingRange/refresh` | 无（`null`） | `()` | proposed |
| `Client::refresh_text_document_content` | `workspace/textDocumentContent/refresh` | `TextDocumentContentRefreshParams` | `()` | proposed |
| `Client::begin_progress` | `window/workDoneProgress/create`，然后发送 `$/progress` begin | `ProgressOptions` | `ProgressHandle` | stable |
| `ProgressHandle::report` | `$/progress` report | `Option<String>` message、`Option<u32>` percentage | `()` | stable |
| `ProgressHandle::end` | `$/progress` end | `Option<String>` message | `()` | stable |

## 需要由应用负责的行为

这些辅助方法只负责协议交互，不隐式提供以下行为：

- **配置缓存。** `configuration` 的结果只交给调用者；`Workspace` snapshot 只通过
  `workspace/didChangeConfiguration` 更新。
- **Diagnostics 缓存。** `publish_diagnostics` 原样发送参数；框架不会存储、去重或
  清除 diagnostics。
- **动态注册状态。** 框架不保存已向客户端注册的 capability 清单；应用需要自行
  跟踪。
- **默认请求超时。** 请求 future 会在客户端响应、返回错误或连接关闭时完成。丢弃
  future 会发送 `$/cancelRequest`。超时需要由应用设置，例如使用
  `tokio::time::timeout`。
- **有界队列。** 出站队列不设上限，也不会丢弃、重排或延迟消息。框架只观察深度：
  超过 [`DEFAULT_OUTBOUND_WARNING_THRESHOLD`](lspf::DEFAULT_OUTBOUND_WARNING_THRESHOLD)
  时，默认值为 1024 且可按服务器配置，引擎会在每次向上跨越阈值时警告一次，并在
  tracing 中记录 `outbound.queue_depth`。
- **Notebook 支持。** 辅助方法中没有 notebook 方法。
- **隐式结束 progress。** 丢弃 [`ProgressHandle`] 会移除 token 并记录警告，但
  不发送消息；只有 `end` 会结束 progress。

[`Client`]: https://docs.rs/lspf/latest/lspf/struct.Client.html
[`Context`]: https://docs.rs/lspf/latest/lspf/struct.Context.html
[`ProgressHandle`]: https://docs.rs/lspf/latest/lspf/struct.ProgressHandle.html
