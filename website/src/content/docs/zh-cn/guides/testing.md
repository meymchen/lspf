---
title: 测试协议行为
description: 使用内存对端和确定性截止时间测试服务器与客户端。
---

在开发依赖中启用仅支持原生目标的 `testing` 功能，就能在不使用套接字或子进程的情况下运行真实 lspf 端点：

```toml
[dev-dependencies]
lspf = { version = "1.0.0", default-features = false, features = ["testing"] }
tokio = { version = "1", features = ["macros", "rt", "time"] }
```

`MemoryTransport::pair` 返回交给端点的 Transport，以及由测试保留的 `ScriptedPeer`。两个方向发送的消息都会复制到同一个 `WireCapture`；从零开始的序号保留消息穿过 Transport 边界时的顺序。

```rust
use std::borrow::Cow;

use bytes::Bytes;
use lspf::testing::{MemoryTransport, WireDirection};
use lspf::{RawMessage, Transport, TransportReader, TransportWriter};

# async fn example() {
let (transport, mut peer) = MemoryTransport::pair();
let capture = peer.capture();
let (mut reader, mut writer) = transport.split();

peer.send(RawMessage::Notification {
    method: Cow::Borrowed("test/inbound"),
    params: Bytes::from_static(b"{}"),
}).unwrap();
assert_eq!(reader.recv().await.unwrap().method(), Some("test/inbound"));

writer.send(RawMessage::Notification {
    method: Cow::Borrowed("test/outbound"),
    params: Bytes::from_static(b"{}"),
}).await.unwrap();
assert_eq!(peer.recv().await.unwrap().method(), Some("test/outbound"));

let traffic = capture.snapshot();
assert_eq!(traffic[0].direction(), WireDirection::PeerToEndpoint);
assert_eq!(traffic[1].direction(), WireDirection::EndpointToPeer);
# }
```

`ServerJourney::start` 驱动 Server 完成 `initialize` 和 `initialized`；`finish` 发送 `shutdown` 与 `exit`，并返回 `Outcome`。`ClientJourney` 为 Client 执行对称流程，并暴露它的 `ServerHandle`。`start_with` 变体接受非默认初始化值；通过 `peer()`，测试可以控制自定义请求、通知、响应和脚本化 Transport 故障。

`VirtualClock::pause` 控制 lspf 为请求和处理器截止时间使用的同一个 Tokio 时钟。它必须在当前线程 Tokio 运行时内创建。先等待脚本对端收到会启动截止时间的消息，再调用 `advance`；时钟跳变会让超时测试保持确定性。

需要覆盖进程边界时，`ci/check-tutorials.sh` 会把完整的[服务端](../tutorials/server)和[客户端](../tutorials/client)教程程序提取到两个独立 Cargo 项目中，以打包后的 lspf crate 构建它们，再让客户端实际驱动服务器。需要精确协议断言和虚拟时间时使用内存 journey；如果要验证进程启动、消息分帧、标准错误或进程回收，至少保留一条受监管的 stdio journey。

## 仓库并发模型

仓库在 `crates/lspf/tests/concurrency_model_support` 中包含一个不依赖第三方库的私有协议会话模型。它会检查六种代表性场景中所有保持顺序的交错执行：

- 带独立标记、乱序返回的响应；
- 完成与取消竞争，以及之后的容量复用；
- 有界队列生产者与发送、关闭竞争；
- 待处理请求和自有任务与重复关闭竞争；
- writer 或必需消息失败与 reader EOF 竞争。

模型会断言关联关系和恰好一次完成、模型中的两条消息与八字节队列上限、费用与入站容量完整释放、只执行一次关闭清理、自有任务得到 join，以及静止前 writer 失败的优先级。这些小上限只是模型边界，不是生产默认值。

使用普通集成测试命令运行全部场景：

```console
cargo test -p lspf --test concurrency_model --no-default-features
```

该测试由 Cargo 自动发现，因此 workspace CI 不需要单独任务。失败信息包含场景名称，以及精确的 actor 与操作序列。搜索顺序是确定的；重新运行相同测试会复现同一条失败轨迹。
