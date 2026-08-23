# 功能、capability 与 workspace

[English](./features-and-workspace.md) | [简体中文](./features-and-workspace.zh-CN.md)

本指南说明 lspf 服务器如何获得标准 LSP 3.17 功能、`ServerCapabilities` 从何而来、
workspace 与文档由谁持有、Command 如何分发，以及如何通过 `FileProvider` 读取未打开
的文件。这里的每段示例都会基于已发布 crate 作为 doctest 编译。完整流程也可以通过
[`crates/lspf-hello`](../../crates/lspf-hello/src/main.rs) 真实运行，并由相邻的 stdio
端到端测试验证。

## 注册功能

注册标准功能只需要一次 builder 调用。[`lspf::features`] 中的描述符会同时确定协议
方法、带类型的参数与结果，以及该功能宣告的 capability。处理器的形式与自定义请求
处理器相同。

```rust
use std::sync::Arc;

use lspf::types::{Hover, HoverParams};
use lspf::{CancellationToken, Context, LspError, Server};

struct State;

async fn hover(
    _state: Arc<State>,
    ctx: Context,
    params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let Some(document) = ctx.documents().get(&uri) else {
        return Ok(None);
    };
    Ok(Some(Hover {
        contents: lspf::types::HoverContents::Scalar(lspf::types::MarkedString::String(
            format!("{} words", document.text().split_whitespace().count()),
        )),
        range: None,
    }))
}

fn main() {
    let server = Server::builder(State)
        // The descriptor fixes `textDocument/hover`, `HoverParams` in, and
        // `Option<Hover>` out — and advertises `hoverProvider: true`.
        .feature(lspf::features::hover(), hover)
        .build()
        .expect("the static registrations are valid");
    // Hand `server` to `lspf::stdio(server).serve()` in a real binary.
}
```

带选项的功能把公开选项传给描述符，服务器会原样宣告这些选项，不需要另外配置
capability：

```rust
# use std::sync::Arc;
# use lspf::types::{CompletionOptions, CompletionParams, CompletionResponse};
# use lspf::{CancellationToken, Context, LspError, Server};
# struct State;
# async fn complete(
#     _state: Arc<State>,
#     _ctx: Context,
#     _params: CompletionParams,
#     _ct: CancellationToken,
# ) -> Result<Option<CompletionResponse>, LspError> {
#     Ok(Some(CompletionResponse::Array(vec![])))
# }
# fn main() {
let server = Server::builder(State)
    .feature(
        lspf::features::completion(CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..CompletionOptions::default()
        }),
        complete,
    )
    .build()
    .expect("the static registrations are valid");
# }
```

依赖功能用于扩展同一功能族。[`completion_resolve()`](lspf::features::completion_resolve)
分发 `completionItem/resolve`，并把宣告的 completion provider 转为带有
`resolveProvider: true` 的选项对象：

```rust
# use std::sync::Arc;
# use lspf::types::{CompletionItem, CompletionOptions, CompletionParams, CompletionResponse};
# use lspf::{CancellationToken, Context, LspError, Server};
# struct State;
# async fn complete(
#     _state: Arc<State>, _ctx: Context, _params: CompletionParams, _ct: CancellationToken,
# ) -> Result<Option<CompletionResponse>, LspError> {
#     Ok(None)
# }
async fn resolve(
    _state: Arc<State>,
    _ctx: Context,
    item: CompletionItem,
    _ct: CancellationToken,
) -> Result<CompletionItem, LspError> {
    Ok(item)
}
# fn main() {
let server = Server::builder(State)
    .feature(lspf::features::completion(CompletionOptions::default()), complete)
    .feature(lspf::features::completion_resolve(), resolve)
    .build()
    .expect("completion and resolve build");
# }
```

非标准方法使用 [`request`](lspf::ServerBuilder::request) 和
[`notification`](lspf::ServerBuilder::notification)，marker type 来自 lspf 重新导出的
[`lspf::types::request`] 与 [`lspf::types::notification`]。自定义方法不会向
`ServerCapabilities` 添加内容，这是绕过封闭功能目录的明确代价。同一个服务器可以
同时注册自定义方法和标准功能。

## 自动派生 capability 与冲突处理

`ServerCapabilities` 与分发路由由同一份注册生成，因此服务器宣告的功能就是实际
提供的功能，builder 也不接受手写的 capability 对象。共用单一 capability 字段的
功能族会把各项贡献合并到该字段，例如 diagnostics、semantic tokens、文件操作以及
各 resolve/prepare 功能族。

注册错误会由 [`build()`](lspf::ServerBuilder::build) 返回静态 [`BuildError`]，不会
拖到运行时，也不会静默采用最后一次写入：

- 同一方法注册两个处理器会返回 `BuildError::DuplicateMethod`；
- 注册框架持有的方法（`initialize`、`shutdown`、`exit`、`initialized`、
  `$/cancelRequest`）会返回 `BuildError::ReservedMethod`；
- 对同一字段提供冲突内容，或缺少基础功能时注册依赖功能，会返回
  `BuildError::ConflictingCapability`。

```rust
# use std::sync::Arc;
# use lspf::types::CompletionItem;
# use lspf::{CancellationToken, Context, LspError, Server};
# struct State;
# async fn resolve(
#     _state: Arc<State>, _ctx: Context, item: CompletionItem, _ct: CancellationToken,
# ) -> Result<CompletionItem, LspError> {
#     Ok(item)
# }
# fn main() {
// Resolve depends on the base completion feature; alone it would advertise a
// dangling `resolveProvider`, so the build fails instead.
let error = match Server::builder(State)
    .feature(lspf::features::completion_resolve(), resolve)
    .build()
{
    Err(error) => error,
    Ok(_server) => panic!("resolve without its base completion feature must fail"),
};
assert_eq!(error, lspf::BuildError::ConflictingCapability { field: "completionProvider" });
# }
```

注册也可以通过唯一一次 `configure_initialize` 事务依赖客户端的 `InitializeParams`。
回调读取只读参数，通过事务型 [`InitializeRegistrar`] 有条件地注册；事务要么全部
提交，要么初始化失败：

```rust
# use std::sync::Arc;
# use lspf::types::{CompletionItem, CompletionParams, CompletionResponse};
# use lspf::{CancellationToken, Context, LspError, Server};
# struct State;
# async fn complete(
#     _state: Arc<State>, _ctx: Context, _params: CompletionParams, _ct: CancellationToken,
# ) -> Result<Option<CompletionResponse>, LspError> {
#     Ok(None)
# }
# async fn resolve(
#     _state: Arc<State>, _ctx: Context, item: CompletionItem, _ct: CancellationToken,
# ) -> Result<CompletionItem, LspError> {
#     Ok(item)
# }
# fn main() {
let server = Server::builder(State)
    .configure_initialize(|params, registrar| {
        let supports_resolve = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|doc| doc.completion.as_ref())
            .and_then(|completion| completion.completion_item.as_ref())
            .is_some_and(|item| item.resolve_support.is_some());
        if supports_resolve {
            registrar.feature(lspf::features::completion_resolve(), resolve);
        }
        Ok(())
    })
    .feature(lspf::features::completion(lspf::types::CompletionOptions::default()), complete)
    .build()
    .expect("the static registrations are valid");
# }
```

## Workspace 与 Documents 的所有权

连接的 [`Workspace`] 和 [`Documents`] 由框架持有，用户状态不持有其中任何一个。
处理器通过 [`Context`] 参数访问二者：`ctx.workspace()` 与 `ctx.documents()`。后者
返回只读 [`DocumentsView`]；只有协议引擎内置的文档同步处理器能够修改底层 store。

`Workspace` 原样保存客户端声明，包括 client info、client capabilities、
initialization options、root URI，以及按声明顺序排列的 workspace folder。后续变更
来自 `workspace/didChangeWorkspaceFolders`、`workspace/didChangeConfiguration` 和
`$/setTrace`。克隆得到的句柄共享同一份状态，因此任何处理器都能看到连接的最新状态：

```rust
# use std::sync::Arc;
# use lspf::{CancellationToken, Context, LspError, Server};
# struct State;
async fn roots(
    _state: Arc<State>,
    ctx: Context,
    _args: Vec<String>,
    _ct: CancellationToken,
) -> Result<Vec<(String, String)>, LspError> {
    // `roots()` prefers the announced folders (multi-root) and falls back to
    // one synthetic root derived from `rootUri`.
    Ok(ctx
        .workspace()
        .roots()
        .into_iter()
        .map(|folder| (folder.uri.as_str().to_string(), folder.name))
        .collect())
}
# fn main() {
#     let server = Server::builder(State)
#         .command("example.roots", roots)
#         .build()
#         .expect("the static registrations are valid");
# }
```

框架跟踪的文档会在用户代码运行前完成同步。为内置文档通知注册处理器时，例如
`textDocument/didOpen`、`didChange`、`didClose`、`willSave` 或 `didSave`，得到的
是该连接唯一的验证后钩子。引擎先解码并修改状态，钩子再通过 `ctx.documents()`
观察结果。

## Command

Command 是在 `workspace/executeCommand` 下按名称分发的带类型闭包。引擎把 command
的 `arguments` 数组解码为 `Args`，tuple、struct 和 `Vec` 都可以；缺少
`arguments` 时按空数组处理。返回值 `Output` 会作为 command 结果发回：

```rust
# use std::sync::Arc;
# use lspf::{CancellationToken, Context, LspError, Server};
# struct State;
async fn count_words(
    _state: Arc<State>,
    ctx: Context,
    args: Vec<String>,
    _ct: CancellationToken,
) -> Result<usize, LspError> {
    let Some(uri) = args.into_iter().next() else {
        return Err(LspError::invalid_params("countWords expects one URI"));
    };
    let uri = uri.parse().map_err(LspError::invalid_params)?;
    let document = ctx
        .workspace()
        .text_document(&uri)
        .await
        .map_err(LspError::invalid_request)?;
    Ok(document.text().split_whitespace().count())
}
# fn main() {
#     let server = Server::builder(State)
#         .command("example.countWords", count_words)
#         .build()
#         .expect("the static registrations are valid");
# }
```

所有名称会合并到一个去重的 `executeCommandProvider`，其中的 `commands` 列表严格
保持注册顺序（ADR 0022），不会受 hash 或后续排序影响。Command 注册错误也属于静态
`BuildError`：空名称为 `EmptyCommandName`，重复名称为 `DuplicateCommand`，同时
注册 Command 与显式 `workspace/executeCommand` 请求处理器则为
`ExecuteCommandConflict`。

## 配置 FileProvider

`ctx.workspace().text_document(uri)` 返回文档 snapshot：优先使用编辑器中已打开的
文本，再查询连接配置的 [`FileProvider`]。provider 由连接的 workspace 持有，通过
builder 配置一次，而且不会缓存；每次 lookup 都会重新查询。框架提供两个实现：

```rust
# use lspf::Server;
# struct State;
# fn main() {
// Native targets: read `file:` URIs from the local filesystem, capped at
// 16 MiB per read by default.
let server = Server::builder(State)
    .file_provider(lspf::OsFileProvider::new())
    .build()
    .expect("the static registrations are valid");

// Or a custom cap through the builder:
let server = Server::builder(State)
    .file_provider(lspf::OsFileProvider::builder().max_bytes(64 * 1024).build())
    .build()
    .expect("the static registrations are valid");
# }
```

[`MemoryFileProvider`] 用于虚拟资源和测试。它的 clone 共享同一个 backing store，
lookup 使用与已打开文档相同的规范化 URI identity：

```rust
# fn main() {
let provider = lspf::MemoryFileProvider::new();
provider.insert(
    "file:///virtual/notes.txt".parse::<lspf::types::Uri>().unwrap(),
    "virtual text",
);
# }
```

失败通过 [`WorkspaceError`] 返回：provider 中没有资源时为 `NotFound`，不支持 URI
scheme 时为 `UnsupportedScheme`，内容不是 UTF-8 时为 `InvalidEncoding`，超过大小
限制时为 `TooLarge`，底层读取失败则为 `Io`。

[`lspf::features`]: https://docs.rs/lspf/latest/lspf/features/
[`lspf::types::request`]: https://docs.rs/lspf/latest/lspf/types/request/
[`lspf::types::notification`]: https://docs.rs/lspf/latest/lspf/types/notification/
[`BuildError`]: https://docs.rs/lspf/latest/lspf/enum.BuildError.html
[`InitializeRegistrar`]: https://docs.rs/lspf/latest/lspf/struct.InitializeRegistrar.html
[`Workspace`]: https://docs.rs/lspf/latest/lspf/struct.Workspace.html
[`Documents`]: https://docs.rs/lspf/latest/lspf/struct.DocumentsView.html
[`DocumentsView`]: https://docs.rs/lspf/latest/lspf/struct.DocumentsView.html
[`Context`]: https://docs.rs/lspf/latest/lspf/struct.Context.html
[`FileProvider`]: https://docs.rs/lspf/latest/lspf/trait.FileProvider.html
[`MemoryFileProvider`]: https://docs.rs/lspf/latest/lspf/struct.MemoryFileProvider.html
[`WorkspaceError`]: https://docs.rs/lspf/latest/lspf/enum.WorkspaceError.html
