# lspf VS Code 测试客户端

[English](./README.md) | [简体中文](./README.zh-CN.md)

这是一个最小的 VS Code 扩展，把 `target/debug/lspf-hello` 作为语言服务器启动，用于
开发期间的手动 smoke test。CI 通过 `lspf-hello` 端到端测试验证同一条路径，命令为
`cargo test -p lspf-hello`，也可以运行 `cargo test --workspace`。

## 设置

需要 Node.js 24。从仓库根目录安装 `package-lock.json` 中的精确版本，编译扩展，再
运行单元测试：

```sh
npm --prefix tools/vscode-test-client ci
npm --prefix tools/vscode-test-client run compile
npm --prefix tools/vscode-test-client test
```

## 启动

在 VS Code 中打开仓库根目录，从 Run and Debug 视图选择
`Debug LSP client (Extension Host)`。pre-launch task 会构建 `lspf-hello` 并以 watch
模式启动 TypeScript compiler。缺少测试客户端依赖时，它会先安装 lock file 中的
版本，因此按 F5 前不需要单独设置或执行 Cargo build。创建或打开任意 `.txt` 文件
后，可以在 `lspf-hello` output channel 中看到 LSP 流量，并在 Extension Host 的
debug console 中看到服务器写到 stderr 的 `tracing` span。

若要运行框架示例，请选择 `Run LSP example client (select example)`。pre-launch task
会构建所有 stdio 示例，picker 会选择 `target/debug/examples/` 下对应的二进制。在
Extension Development Host 中打开 `.txt` 文件，即可发送真实编辑器请求。

调试 Rust breakpoint 时，保持 Extension Development Host 运行，在仓库窗口启动
`Attach to running LSP server/example`，然后选择名称与示例相同的进程。CodeLLDB 会
连接当前持有 stdio 连接的进程。

默认设置 `RUST_LOG=lspf=trace` 与 `LSPF_LOG_FORMAT=json`。`lspf-hello` output
channel 每行接收一个 JSON event。启动 VS Code 前可以 export 任一变量覆盖默认值；
使用 `LSPF_LOG_FORMAT=text` 可切换为紧凑文本。

## Command

语言客户端初始化后，打开 Command Palette，在 `lspf hello` 分类中选择：

- `Show Workspace Roots`
- `Read Active File`
- `Run Outgoing Helper Journey`
- `Run Cancellable Progress`

`vscode-languageclient` 会自动注册服务器通过 `executeCommandProvider` 宣告的
Command；`package.json` 只负责提供 Command Palette 中显示的标题。需要文档 URI
时，`executeCommand` middleware 会加入当前编辑器的 URI。结果写入
`lspf-hello commands` output channel。outgoing journey 会调用
`workspace/applyEdit`，在当前文档开头插入一行注释。

## 验证范围

该客户端验证真实编辑器在协议层对服务器提出的要求：VS Code 的 `initialize`
payload 可以反序列化为 `lsp_types::InitializeParams`；生成的
`ServerCapabilities` 宣告增量文档同步；响应、随后的 `didOpen` 与服务器发布的
diagnostic 都能通过 stdio framing 往返。
