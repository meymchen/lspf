---
title: API 参考
description: 带版本的 API 文档与项目参考资料。
---

需要准确签名和功能可用性时，请使用带版本的 Rust API 参考。

## Rust API

- [docs.rs 上的最新 lspf API](https://docs.rs/lspf)
- [crates.io 上的 crate 版本](https://crates.io/crates/lspf)
- [冻结的 1.0 公开接口](https://github.com/meymchen/lspf/blob/main/docs/public-interface.md)

## 任务指南

- [错误与取消](guides/errors-and-cancellation)
- [注册服务端功能](guides/features-and-workspace)
- [管理工作区状态](guides/workspace-state)
- [调用编辑器](guides/outgoing-client)
- [报告进度与自定义消息](guides/progress-and-custom-messages)
- [选择传输层](guides/transports)
- [使用 stdio 与自定义传输层](guides/stdio-and-custom-transports)
- [构建 LSP 客户端](guides/client-adoption)
- [协议测试](guides/testing)
- [资源与可观测性策略](guides/operations)
- [部署与故障排查](guides/deployment-and-troubleshooting)
- [功能示例服务器](examples)

## 架构与支持

- [领域模型](https://github.com/meymchen/lspf/blob/main/CONTEXT.md)
- [架构决策记录](https://github.com/meymchen/lspf/tree/main/docs/adr)
- [支持与安全策略](https://github.com/meymchen/lspf/blob/main/SECURITY.md)
- [发布历史](https://github.com/meymchen/lspf/blob/main/crates/lspf/CHANGELOG.md)

::: info 注意版本
仓库可能包含计划在下一版本发布的 API。需要已发布版本的精确接口时，请查看与你的 `Cargo.lock` 版本对应的 docs.rs 页面。
:::
