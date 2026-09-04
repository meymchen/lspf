---
title: 注册服务端功能
description: 注册类型化 LSP 功能，并从同一份声明中推导能力。
---

## 功能注册

功能描述符提供方法、参数与结果类型以及能力元数据。注册会把它连接到你的处理器：

```rust
let server = Server::builder(state)
    .feature(lspf::features::hover(), hover)
    .feature(lspf::features::completion(options), complete)
    .command("acme.organize", organize)
    .build()?;
```

构建器会拒绝相互冲突的注册。初始化阶段返回的能力来自已经构建的功能目录，而不是另一份手写结构。

同一标准功能、原始方法或命令不能重复注册。需要定制通告选项时，把选项传给描述符；只需处理消息时，使用默认描述符。原始 `request` 和 `notification` 路由适合扩展方法，但不会自动通告标准能力。

## LSP 3.18 新增功能

lspf 为 LSP 3.18 类型和已实现功能提供类型化描述符，包括内联补全、文档诊断与工作区诊断、笔记本文档同步、文件操作、刷新请求和进度。是否启用仍取决于注册：协议类型存在不代表服务器已经声明或实现该能力。

客户端能力也会影响最终结果。初始化后应从 `ServerContext` 读取协商结果，不要假设客户端支持某个位置编码、动态注册或刷新请求。

## 自动推导能力与冲突

`build()` 会从功能目录生成 `ServerCapabilities`。这保证通告的方法与实际路由来自同一注册；重复方法、互斥能力和重复命令会在 I/O 开始前以 `BuildError` 失败。需要完全自定义 initialize 结果的扩展仍应避免与框架推导的标准字段冲突。

接下来阅读[管理工作区状态](../workspace-state/)，了解同步文档、笔记本、命令和未打开文件。
