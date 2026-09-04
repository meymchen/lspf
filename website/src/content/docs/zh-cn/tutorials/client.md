---
title: 驱动语言服务器
description: 通过 lspf 的类型化客户端端点启动并控制语言服务器。
---

lspf 也可以充当 LSP 客户端。本教程会把服务器作为受监管的 stdio 子进程启动，发送类型化消息，并干净地关闭进程。

## 1．添加客户端依赖

```toml title="Cargo.toml"
[dependencies]
lspf = "1.0.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "process"] }
```

## 2．注册反向消息处理器

服务器可以反向向客户端发送请求和通知。请在建立连接前注册相应处理器：

```rust
use lspf::types::ClientCapabilities;
use lspf::types::notification::PublishDiagnostics;
use lspf::Client;

let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
let client = Client::builder(ClientCapabilities::default())
    .notification::<PublishDiagnostics, _, _>(move |_ctx, params| {
        let _ = tx.send(params);
        async {}
    });
```

## 3．启动子进程

```rust
let child = client
    .spawn(tokio::process::Command::new("lspf-tutorial-server"))
    .await?;
let server = child.server();
```

`spawn` 会配置协议管道，执行 `initialize` 和 `initialized`，启动入站驱动，并在服务器就绪后返回。

## 4．发送类型化消息

`ServerHandle::notify` 和 `ServerHandle::request` 以协议标记类型作为类型参数。标记类型同时确定方法、参数和结果类型，因此不需要字符串方法名或响应类型转换。

## 5．明确关闭所有权

```rust
let result = child.shutdown().await?;
eprintln!("server exited with {:?}", result.status());
```

子连接拥有协议驱动、进程和有大小上限的标准错误缓冲区。应由负责关闭流程的任务持有它；其他需要发送调用的任务可以复制 `server`。

`shutdown` 会依次发送 `shutdown` 和 `exit`，等待协议结束并回收进程。返回值包含 `Outcome`、操作系统退出状态和最多 64 KiB 的 stderr；即使捕获区已满，stderr 仍会继续排空，以免子进程死锁。预计服务器自行退出时改用 `wait`。

## 6．组合完整程序

完整程序应按以下顺序组合前面的代码：从唯一命令行参数读取服务器路径；创建诊断通道并注册 `PublishDiagnostics`；用该路径构造 `Command` 并调用 `spawn`；发送 `DidOpenTextDocument`；等待并校验诊断；通过 `HoverRequest` 请求悬停；通过 `ExecuteCommand` 调用服务器注册的命令；最后调用 `child.shutdown()`，检查 `Outcome::Exit { code: 0 }`、进程状态、stderr 和 `stderr_truncated()`。

等待诊断时应使用有限超时。通知只等待进入发送队列；请求会等待按 ID 关联的类型化响应。若任一步失败，仍要让唯一所有者回收子进程，不能只丢弃 `ServerHandle`。

构建客户端与[服务器教程](../server/)中的项目后运行：

```console
cargo run -- /absolute/path/to/lspf-tutorial-server
```

没有输出表示所有断言通过。设置 `RUST_LOG=lspf=trace` 可以让子进程把 lspf 协议事件写到 stderr。

## 下一步

- [客户端接入](../../guides/client-adoption/)进一步介绍自定义 Transport、反向请求、截止时间、提前退出和连接所有权。
- [协议测试](../../guides/testing/)使用确定性的内存对端替换子进程。
- [错误与取消](../../guides/errors-and-cancellation/)说明端点错误类型和取消路径。
- [资源与可观测性策略](../../guides/operations/)介绍预算与遥测；[部署与故障排查](../../guides/deployment-and-troubleshooting/)介绍进程拓扑、关闭与故障诊断。
