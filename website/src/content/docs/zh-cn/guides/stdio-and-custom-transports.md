---
title: 使用 stdio 与自定义传输层
description: 遵循 stdio 所有权规则，或为嵌入式宿主实现消息帧传输层。
---

选定连接形态后，可以用本指南处理 stdio 的所有权细节，或为特殊宿主实现自己的消息帧传输层。

## Stdio 规则

stdio 只适用于启用 `stdio` 的原生目标。lspf 按 `Content-Length: N\r\n\r\n` 读写二进制 LSP 消息；stdout 不能包含任何日志或横幅，所有日志必须写到 stderr。`lspf::stdio(server).serve().await` 返回 `Outcome`，不会终止进程；EOF、对端关闭、协议 `exit` 和致命初始化错误都经同一结果路径返回。

### 启动语言服务器子进程

原生 Client 可以用 `ClientBuilder::spawn` 监督任意 stdio 命令。它会接管三个标准流、初始化连接、驱动入站流量并并发排空 stderr。`shutdown` 发送 LSP shutdown 与 exit 后回收进程；`wait` 用于预计会自行退出的进程。两者都返回协议结果、OS 状态和最初 64 KiB stderr，之后仍持续排空以避免死锁。

子进程不退出时会经过有界等待、terminate、再次等待和 kill。丢弃存活连接仍会把资源交给回收线程；显式 `shutdown` 或 `wait` 才能让应用取得终止证据。

## 实现自定义 Transport

为消息帧通道实现 `Transport`、`TransportReader` 和 `TransportWriter`。`recv` 每次必须产生一个完整、已解码的 `RawMessage`，不能暴露部分字节或合并信封；`send` 每次编码一个消息并维持调用顺序；拆分后的读写半部可以并行工作；`shutdown` 应刷新已接收输出、发送协议关闭并释放通道。

字节流通常添加和移除 `Content-Length`，已经分帧的通道则让一条通道消息对应一个 `RawMessage`。分配大正文前要实施有限大小限制。普通 EOF、对端关闭和向已关闭对端写入使用 `TransportError::Closed`；无效分帧用 `Malformed`，超过上限用 `OversizedMessage`，有意义的 I/O 源用 `Io`，JSON 转换用 `Serde`。适配器不得在连接边界之下偷偷重试或重连。

TLS、mTLS、ALPN、证书轮换和身份验证属于应用：先建立并验证安全流，再在其上实现 Transport。

## Transport 的范围

内置 Transport 不提供 TLS 配置、多客户端服务、WebSocket 客户端模式、重连、CLI 传输选择、笔记本／客户端框架或共享内存 WASM。这些是部署或客户端框架策略，应在 lspf 外实现；只有消息分帧契约适合抽象时才实现自定义 Transport。
