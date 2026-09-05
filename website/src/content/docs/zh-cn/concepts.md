---
title: 核心概念
description: 理解 lspf 的所有权与分发模型。
---

<!-- markdownlint-disable-next-line MD025 -->
# 核心概念

lspf 在应用状态与单连接协议状态之间划出明确边界。理解这条边界后，其余 API 就会变得直观。

## 消息路径与所有权

<!-- markdownlint-disable-next-line MD033 -->
<ArchitectureFlow />

协议引擎处理生命周期、取消与状态同步。用户 Layer 包裹用户分发流程，不能拦截协议内建的状态变更。对于文档通知，引擎先应用变更，再通过 Service 栈调用已注册的变更后钩子。

固定栈由外到内依次为 panic 隔离、tracing、有界并发、用户 Layer 和 Router service。最后注册的用户 Layer 位于最外侧用户位置。Router 在初始化期间合并静态注册与初始化条件注册后冻结。

连接私有的协议会话负责请求关联、资源准入、截止时间、出站队列与任务清理。`ServerContext` 提供只读视图与类型化句柄；`Arc<State>` 保存应用拥有的数据。

这些路径对应 [`ProtocolEngine::dispatch`](https://github.com/meymchen/lspf/blob/main/crates/lspf/src/engine.rs)、[`build_service_stack`](https://github.com/meymchen/lspf/blob/main/crates/lspf/src/service.rs) 和私有的 [`ProtocolSession`](https://github.com/meymchen/lspf/blob/main/crates/lspf/src/session.rs) 实现。

## Server 与处理器

一个 `Server` 只拥有一条 LSP 连接。`Server::builder(state)` 用于注册类型化请求处理器、通知处理器、命令、生命周期钩子和服务层。构建完成后，再通过一种传输层为它提供服务。

处理器以 `Arc<State>` 接收应用状态，同时获得一个复制成本很低的 `ServerContext`。请求处理器还会收到 `CancellationToken`。

## ServerContext

`ServerContext` 是访问当前连接中框架状态的入口：

- `ctx.documents()` 读取已经同步的文本文档。
- `ctx.notebooks()` 读取笔记本结构。
- `ctx.workspace()` 读取工作区文件夹和配置。
- `ctx.client()` 向编辑器发送类型化请求和通知。
- `ctx.partial_results()` 为支持的请求分批报告结果。

不要把上下文或它的视图存进全局应用状态。使用当前调用收到的值，可以让连接所有权保持明确。

## 功能与能力声明

功能描述符把 LSP 方法与参数、结果类型绑定在一起。注册描述符时，它也会为初始化阶段贡献能力声明。相互冲突的注册会成为构建错误，而不会悄悄以后注册者覆盖前者。

```rust
Server::builder(state)
    .feature(lspf::features::hover(), hover)
    .feature(lspf::features::completion(options), completion)
    .build()?;
```

## 文档与工作区

文档同步是内置协议行为。lspf 会先应用 `didOpen`、`didChange` 和 `didClose`，再运行你的变更后钩子。处理器通过 `DocumentsView` 读取不可变、基于 rope 的文档快照。

`Workspace` 集中保存多根工作区文件夹、初始化值、配置和文档访问能力。需要解析编辑器尚未打开的资源时，可以配置 `FileProvider`。

## 并发与取消

用户请求和通知处理器会在有限资源策略下并发运行。协议内建的文档变更在读取循环中执行，先于对应钩子。收到 `$/cancelRequest` 后，框架会触发该请求的 `CancellationToken`。取消可以停止异步工作，但不会回滚副作用，也不会停止已经启动的阻塞任务。阻塞任务仍应单独设置并发上限。
