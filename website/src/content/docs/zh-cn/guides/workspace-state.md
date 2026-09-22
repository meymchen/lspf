---
title: 管理工作区状态
description: 使用同步文档、笔记本、命令和未打开文件。
---

服务器的工作区状态限定在单条连接内，并通过 `ServerContext` 提供。本指南介绍服务端实现中与状态相关的部分。

## Workspace 与 Documents 的所有权

每个处理器都会收到 `ServerContext`。它的视图暴露由协议引擎持续更新的状态：

```rust
let documents = ctx.documents();
let workspace = ctx.workspace();
let client = ctx.client();
```

这些对象属于单条连接。文档视图用于取得不可变的 `Document` 快照；打开、变更和关闭通知会原子更新框架状态。工作区文件夹、初始化选项、设置和跟踪级别会随协议事件更新。处理器不应把视图长期保存到连接之外，也不应另建一份可能漂移的打开文档表。

位置换算必须使用协商后的编码和文档视图辅助方法。变更版本或资源预算校验失败时，旧快照保持不变，变更后的钩子不会运行。

## 读取一个 Document 快照中的文本

相关查询应先取得同一个 `Document`，再把连接的 `DocumentsView::position_encoding()` 传给需要位置的方法。这些读取始终使用该不可变快照；即使之后收到 `didChange` 或 `didClose`，保留的快照仍可读取原文。笔记本单元格和 `Workspace::text_document` 返回的快照使用同一套接口；提供器加载的快照保持 `version() == None`，语言标识为空。

```rust
# use lspf::{ServerContext, types::{Position, Range, Uri}};
# fn inspect(ctx: ServerContext, uri: Uri, selection: Range, cursor: Position) {
let documents = ctx.documents();
let encoding = documents.position_encoding();
if let Some(document) = documents.get(&uri) {
    let line = document.line(cursor.line);
    let selected = document.text_in_range(encoding, selection);
    let word = document.word_at_position(encoding, cursor, |ch| {
        ch.is_alphanumeric() || ch == '_' || ch == '\u{301}'
    });
    // `word` 包含文本和范围，范围使用传入的编码。
}
# }
```

`Document::line(line)` 接受从零开始的行号，返回 `Option<Cow<'_, str>>`。结果去掉完整的行终止符，保留尾部空格等其余字符。现有坐标模型识别 LF、CRLF、CR、VT、FF、NEL（U+0085）、行分隔符（U+2028）和段落分隔符（U+2029）。空文档有一行空行；以行终止符结尾的文档还有最后一行空行。不存在的行返回 `None`。行号沿用快照现有的位置坐标模型。

`Document::text_in_range(encoding, range)` 以 `Option<Cow<'_, str>>` 返回包含起点、不包含终点的原始文本，保留跨越的行终止符和 Unicode 字符，不做规范化。终点为下一行第零列时，结果包含前一行的终止符。有效的空范围返回 `Some("")`，文档末尾也一样。

`Document::word_at_position(encoding, position, predicate)` 返回 `Option<(Cow<'_, str>, Range)>`。词是同一行内由谓词接受的 Unicode 标量值组成的最长连续片段。优先选择光标紧右侧字符所属的词；该字符不被接受时，再检查紧左侧字符。因此光标位于词尾时会选中该词，但不会隔着空白向前搜索。即使谓词接受行终止符，选词也不会跨行。谓词仅用于本次调用，由应用决定是否接受下划线、连字符和组合标记；它不负责标识符校验或字素簇分割。

两个位置方法都会以 `None` 拒绝不存在的行、超过行内容的列、位于 UTF-8 字符或 UTF-16 代理对内部的位置，以及位于行终止符内部的位置。紧靠终止符之前的行尾位置有效。范围读取还会拒绝逆序端点和坐标无效的空范围；光标相邻两侧均没有被接受的字符时，选词返回 `None`。无效输入不会被截断，也不会修改快照。已有全文读取及坐标换算方法保持原有契约。

结果在可能时借用保留的 `Document`，否则持有所选文本；借用只是优化，不是保证。结果不持有存储锁，也不暴露底层存储类型。局部查询可能为片段或坐标换算所需的一行分配内存，但不会先复制整个文档。读取不会访问提供器，也不会更改元数据或工作区状态。

## 笔记本同步

注册笔记本同步后，lspf 会维护笔记本元数据及其单元格文档。单元格计入 `max_documents` 和 `max_document_bytes`，笔记本本身计入 `max_notebooks`。笔记本打开、结构变化、单元格文本变化和关闭都有独立钩子；单元格事件不会冒充普通文本文档钩子。

过滤器决定服务器接纳哪些笔记本。应用应在钩子中维护语言层索引，但把协议快照交给框架管理。

## 命令

命令按名称注册，并在 `workspace/executeCommand` 下分派。名称会按注册顺序自动加入能力声明。处理器负责验证 JSON 参数、检查取消令牌并返回可序列化结果；未知命令和无效参数会成为协议错误。

## FileProvider 配置

请求要读取编辑器尚未打开的文件时，可以配置 `FileProvider`。打开文档始终优先使用内存快照，以免磁盘内容覆盖未保存编辑。提供器负责 URI 到存储的映射、I/O、大小限制和访问策略；失败应转换成适合用户的 `LspError`，不要把宿主内部路径或凭据写入响应。

处理器通过 `ctx.client()` 取得的 `ClientHandle` 可以发送诊断、消息、配置请求、动态注册、工作区编辑、进度和刷新请求。所有调用共享连接的有界出站资源，并能感知连接关闭；需要完整调用方式时参见[向客户端发送请求](outgoing-client)。

`ClientHandle` 为诊断、消息、配置、动态注册、编辑、进度和刷新请求提供类型化辅助方法。所有调用共享连接的有界出站资源，并能感知连接关闭。
