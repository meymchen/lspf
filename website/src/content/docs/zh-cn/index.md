---
title: lspf：立足 IDE，扩展至 Agent 的语言工具框架
description: 用 Rust 为 IDE 构建语言服务器，再通过类型化 LSP 客户端将语言能力扩展到 Agent 工具。
layout: home
editLink: false
lastUpdated: false
hero:
  name: lspf
  text: 立足 IDE，<br>扩展至 Agent。
  tagline: 从编辑器每天依赖的语言功能出发。用类型化 Rust 服务器接入 IDE，再通过同一套语言服务器协议，将语言能力扩展到 Agent 工具。
  actions:
    - theme: brand
      text: 开始构建
      link: /zh-cn/getting-started
    - text: 在 GitHub 查看
      theme: alt
      link: https://github.com/meymchen/lspf
features:
  - title: 从处理器到协议线都类型安全
    details: 使用 Rust 类型注册协议功能。lspf 从同一份注册信息推导能力声明，让行为与元数据始终一致。
  - title: 内置协议状态管理
    details: 通过处理器收到的上下文读取已同步文档、笔记本、工作区根目录、配置和客户端连接。
  - title: 异步、有界、可取消
    details: 在有限资源策略下并发处理请求，并用取消令牌停止工作；API 从一开始就面向生产环境的资源所有权。
  - title: 选择合适的传输层
    details: 可直接使用 stdio、TCP、WebSocket 或浏览器与 Node Worker；也可以实现公开的消息帧传输 trait。
---

## 以 IDE 为基础，向 Agent 扩展

从 IDE 开始：在 lspf 服务器中实现悬停、补全、诊断等语言功能。框架负责 LSP 生命周期、文档同步、能力声明与取消，处理器负责语言分析。

再通过 lspf 的类型化 `Client` 扩展这份基础：Agent 宿主可以连接语言服务器，复用它的 LSP 功能。工具选择、模型调用以及是否应用编辑的决策，仍由宿主应用负责。

<!-- markdownlint-disable-next-line MD033 -->
<ArchitectureFlow />

[探索服务器架构](./concepts) · [构建客户端连接](./guides/client-adoption)

## 小而清晰的 API 边界

```rust
let server = Server::builder(State)
    .feature(lspf::features::hover(), hover)
    .feature(lspf::features::completion(options), complete)
    .command("acme.organize", organize)
    .build()?;

let outcome = lspf::stdio(server).serve().await?;
```

框架负责 JSON-RPC、初始化、文档同步、能力声明、取消和关闭流程。你的处理器只需接收类型化参数并返回类型化结果。

[安装 lspf 并构建第一个服务器 →](./getting-started)

## 按需求选择文档

- 第一次使用 lspf：从[开始使用](./getting-started)入手，然后完成[服务器教程](./tutorials/server)。
- 正在构建服务器：先阅读[功能注册](./guides/features-and-workspace)和[工作区状态](./guides/workspace-state)，再按需加入[编辑器调用](./guides/outgoing-client)或[进度报告](./guides/progress-and-custom-messages)。
- 正在连接或发布服务器：先[选择传输层](./guides/transports)，再查阅[客户端连接](./guides/client-adoption)、[测试](./guides/testing)和[生产策略](./guides/operations)指南。
- 想直接阅读可运行代码：选择一个小型[功能示例服务器](./examples)。
- 需要准确签名：查看带版本的 [API 参考](./reference)。

## lspf 支持范围

稳定功能目录覆盖 LSP 3.18 请求与通知。原生服务器可以使用 stdio、TCP 或 WebSocket；浏览器和 Node Worker 使用 Worker Channel；嵌入式宿主可以实现公开传输 trait。框架还提供类型化出站客户端、文档与笔记本同步、工作区状态、有界并发、取消、进度和协议测试工具。
