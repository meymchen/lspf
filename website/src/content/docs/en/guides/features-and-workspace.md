---
title: Register server features
description: Register typed LSP features and derive capabilities from the same declarations.
---

This guide covers how a lspf server gains stable LSP 3.18 features, where
its `ServerCapabilities` come from, who owns the workspace and the documents,
how Commands dispatch, and how unopened files resolve through a
`FileProvider`. Every example here compiles as a doctest against the shipped
crate. The public interfaces are exercised by the framework's
[integration tests](https://github.com/meymchen/lspf/tree/main/crates/lspf/tests),
while the
[`lspf-markdown` reference server](https://github.com/meymchen/lspf/tree/main/crates/lspf-markdown)
drives the same document, workspace, feature, and stdio boundaries with real
Markdown behavior. For a project starter, use
[`lspf-vscode-extension-template`](https://github.com/meymchen/lspf-vscode-extension-template).

## Feature registration

A standard feature is registered with one builder call: a descriptor from
[`lspf::features`] fixes the wire method, the typed parameters and result, and
the capability the feature advertises — all at once. The handler has the same
shape as a custom request handler.

```rust
use std::sync::Arc;

use lspf::types::{Hover, HoverParams};
use lspf::{CancellationToken, ServerContext, LspError, Server};

struct State;

async fn hover(
    _state: Arc<State>,
    ctx: ServerContext,
    params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let Some(document) = ctx.documents().get(&uri) else {
        return Ok(None);
    };
    Ok(Some(Hover {
        contents: lspf::types::HoverContents::MarkupContent(lspf::types::MarkupContent {
            kind: lspf::types::MarkupKind::PlainText,
            value: format!("{} words", document.text().split_whitespace().count()),
        }),
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

Options-carrying features take their public options in the descriptor, and
the same options are what the server advertises — there is no separate
capability knob:

```rust
# use std::sync::Arc;
# use lspf::types::{CompletionOptions, CompletionParams, CompletionResponse};
# use lspf::{CancellationToken, ServerContext, LspError, Server};
# struct State;
# async fn complete(
#     _state: Arc<State>,
#     _ctx: ServerContext,
#     _params: CompletionParams,
#     _ct: CancellationToken,
# ) -> Result<Option<CompletionResponse>, LspError> {
#     Ok(Some(CompletionResponse::CompletionItemList(vec![])))
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

Dependent features extend a family. [`completion_resolve()`](lspf::features::completion_resolve)
dispatches `completionItem/resolve` and turns the advertised completion
provider into an options object carrying `resolveProvider: true`:

```rust
# use std::sync::Arc;
# use lspf::types::{CompletionItem, CompletionOptions, CompletionParams, CompletionResponse};
# use lspf::{CancellationToken, ServerContext, LspError, Server};
# struct State;
# async fn complete(
#     _state: Arc<State>, _ctx: ServerContext, _params: CompletionParams, _ct: CancellationToken,
# ) -> Result<Option<CompletionResponse>, LspError> {
#     Ok(None)
# }
async fn resolve(
    _state: Arc<State>,
    _ctx: ServerContext,
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

Non-standard methods use [`request`](lspf::ServerBuilder::request) and
[`notification`](lspf::ServerBuilder::notification) with the marker types
lspf re-exports as [`lspf::types::request`] and [`lspf::types::notification`].
Custom methods add nothing to `ServerCapabilities` — that is the price of
escaping the sealed catalog — so a custom method and a standard feature are
both valid for the same server.

## The LSP 3.18 additions

Three inbound request methods arrived with LSP 3.18. They register through the
same `feature` call as everything else; only their capability contributions are
worth calling out.

[`inline_completion(options)`](lspf::features::inline_completion) dispatches
`textDocument/inlineCompletion` and advertises `inlineCompletionProvider`:

```rust
# use std::sync::Arc;
use lspf::types::{
    InlineCompletionItem, InlineCompletionOptions, InlineCompletionParams,
    InlineCompletionResponse, InsertText,
};
# use lspf::{CancellationToken, ServerContext, LspError, Server};
# struct State;
async fn inline_completion(
    _state: Arc<State>,
    _ctx: ServerContext,
    _params: InlineCompletionParams,
    _ct: CancellationToken,
) -> Result<Option<InlineCompletionResponse>, LspError> {
    Ok(Some(
        vec![InlineCompletionItem::new(
            InsertText::String("println!()".to_string()),
            None,
            None,
            None,
        )]
        .into(),
    ))
}
# fn main() {
let server = Server::builder(State)
    .feature(
        lspf::features::inline_completion(InlineCompletionOptions::default()),
        inline_completion,
    )
    .build()
    .expect("the static registrations are valid");
# }
```

[`text_document_content(options)`](lspf::features::text_document_content)
dispatches `workspace/textDocumentContent`, which serves virtual documents the
client cannot read from disk. The options name the URI schemes the server
answers for, and land under `workspace.textDocumentContent` in the advertised
capabilities:

```rust
# use std::sync::Arc;
use lspf::types::{TextDocumentContentOptions, TextDocumentContentParams, TextDocumentContentResult};
# use lspf::{CancellationToken, ServerContext, LspError, Server};
# struct State;
async fn text_document_content(
    _state: Arc<State>,
    _ctx: ServerContext,
    params: TextDocumentContentParams,
    _ct: CancellationToken,
) -> Result<TextDocumentContentResult, LspError> {
    Ok(TextDocumentContentResult::new(format!("// generated for {}", params.uri)))
}
# fn main() {
let server = Server::builder(State)
    .feature(
        // Only `lspf:` URIs route here; the client reads every other scheme
        // the way it normally would.
        lspf::features::text_document_content(TextDocumentContentOptions::new(vec![
            "lspf".to_string(),
        ])),
        text_document_content,
    )
    .build()
    .expect("the static registrations are valid");
# }
```

[`ranges_formatting(options)`](lspf::features::ranges_formatting) dispatches
`textDocument/rangesFormatting`. It has no capability field of its own: it
merges into the same `documentRangeFormattingProvider`
[`range_formatting`](lspf::features::range_formatting) contributes to, and
registering it is what sets `rangesSupport: true` there. Register the two
together so a client that only knows single-range formatting still has a route:

```rust
# use std::sync::Arc;
use lspf::types::{
    DocumentRangeFormattingOptions, DocumentRangeFormattingParams,
    DocumentRangesFormattingParams, TextEdit,
};
# use lspf::{CancellationToken, ServerContext, LspError, Server};
# struct State;
# async fn range_formatting(
#     _state: Arc<State>,
#     _ctx: ServerContext,
#     _params: DocumentRangeFormattingParams,
#     _ct: CancellationToken,
# ) -> Result<Option<Vec<TextEdit>>, LspError> {
#     Ok(None)
# }
async fn ranges_formatting(
    _state: Arc<State>,
    _ctx: ServerContext,
    _params: DocumentRangesFormattingParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<TextEdit>>, LspError> {
    Ok(None)
}
# fn main() {
let server = Server::builder(State)
    .feature(
        lspf::features::range_formatting(DocumentRangeFormattingOptions::default()),
        range_formatting,
    )
    .feature(
        lspf::features::ranges_formatting(DocumentRangeFormattingOptions::default()),
        ranges_formatting,
    )
    .build()
    .expect("range and ranges formatting build as one family");
# }
```

The corresponding outgoing 3.18 additions — `workspace/foldingRange/refresh`
and `workspace/textDocumentContent/refresh` — live on `ClientHandle` and are
covered by the
[outgoing client guide](outgoing-client#workspace-refresh).

## Automatic capability derivation and conflicts

`ServerCapabilities` are generated from the same registrations that dispatch:
what the server advertises is exactly what it serves, and no builder call
accepts a handwritten capability object. Families that share one singular
capability field (diagnostics, semantic tokens, file operations, each
resolve/prepare family) merge their contributions under that field.

Mistakes are static [`BuildError`]s from [`build()`](lspf::ServerBuilder::build),
never a runtime surprise or a silent last-write-wins:

- two handlers for one method → `BuildError::DuplicateMethod`;
- a method the framework owns (`initialize`, `shutdown`, `exit`,
  `initialized`, `$/cancelRequest`) → `BuildError::ReservedMethod`;
- two contributions that disagree on a singular field, or a dependent feature
  without its base → `BuildError::ConflictingCapability`.

```rust
# use std::sync::Arc;
# use lspf::types::CompletionItem;
# use lspf::{CancellationToken, ServerContext, LspError, Server};
# struct State;
# async fn resolve(
#     _state: Arc<State>, _ctx: ServerContext, item: CompletionItem, _ct: CancellationToken,
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

Registrations can depend on the client's `InitializeParams` through the one
`configure_initialize` transaction. The callback sees read-only parameters
and a transactional [`InitializeRegistrar`], registers conditionally, and
either commits the whole transaction or fails initialization:

```rust
# use std::sync::Arc;
# use lspf::types::{CompletionItem, CompletionParams, CompletionResponse};
# use lspf::{CancellationToken, ServerContext, LspError, Server};
# struct State;
# async fn complete(
#     _state: Arc<State>, _ctx: ServerContext, _params: CompletionParams, _ct: CancellationToken,
# ) -> Result<Option<CompletionResponse>, LspError> {
#     Ok(None)
# }
# async fn resolve(
#     _state: Arc<State>, _ctx: ServerContext, item: CompletionItem, _ct: CancellationToken,
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

Continue with [Workspace state](workspace-state) to work with synchronized documents, notebooks, commands, and unopened files.

[`lspf::features`]: https://docs.rs/lspf/latest/lspf/features/
[`lspf::types::request`]: https://docs.rs/lspf/latest/lspf/types/request/
[`lspf::types::notification`]: https://docs.rs/lspf/latest/lspf/types/notification/
[`BuildError`]: https://docs.rs/lspf/latest/lspf/enum.BuildError.html
[`InitializeRegistrar`]: https://docs.rs/lspf/latest/lspf/struct.InitializeRegistrar.html
