---
title: 调用编辑器
description: 从服务器向已连接的客户端发送类型化通知与请求。
---

语言服务器经常需要主动联系编辑器，例如发布诊断、读取配置、应用编辑或显示进度。每个服务端处理器都可以通过 `ctx.client()` 获得 `ClientHandle`。

## 发送通知

通知只等待进入出站队列，不等待客户端响应：

```rust
ctx.client().show_message(ShowMessageParams {
    typ: MessageType::Info,
    message: "Index ready".into(),
})?;
```

发送可能因为队列达到上限或连接已经关闭而失败。不要忽略返回的 `ClientError`。

## 窗口与工作区请求

配置、工作区编辑和动态注册等操作会等待类型化响应：

```rust
let values = ctx.client().configuration(params).await?;
```

`show_document`、`show_message_request`、`apply_edit`、`configuration` 和 `workspace_folders` 都是对应协议方法的薄封装。它们会原样发送参数，并返回类型化结果，不缓存配置、不修改 `Workspace` 快照，也不替应用决定是否接受编辑。

出站请求与连接共享消息数和编码字节预算。默认截止时间是 30 秒；可通过 `ResourcePolicy::outbound_request_timeout` 修改，只有另有明确截止时间时才设为 `None`。调用方不再需要结果时，丢弃 future 会移除待处理项并尝试发送一次 `$/cancelRequest`。

## 动态注册

`register_capability` 和 `unregister_capability` 向支持动态注册的客户端通告变化。调用前检查初始化能力，并由应用记录已注册的 ID、方法和选项；框架不会维护动态注册清单。重新注册、重连和回滚策略也属于应用。

## 工作区刷新

`refresh_code_lenses`、`refresh_diagnostics`、`refresh_inlay_hints`、`refresh_inline_values`、`refresh_semantic_tokens`、`refresh_folding_ranges` 和 `refresh_text_document_content` 会请求客户端重新拉取相应数据。只有客户端能力明确支持时才调用；这些方法不会主动重新计算或缓存结果。

接下来阅读[报告进度与自定义消息](progress-and-custom-messages)，了解长时间运行的工作、分批结果和协议扩展。
