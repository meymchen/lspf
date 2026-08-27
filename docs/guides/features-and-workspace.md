# Features, capabilities, and the workspace

This guide covers how a lspf server gains standard LSP 3.17 features, where
its `ServerCapabilities` come from, who owns the workspace and the documents,
how Commands dispatch, and how unopened files resolve through a
`FileProvider`. Every example here compiles as a doctest against the shipped
crate, and the complete journey runs as a real server in
[`crates/lspf-hello`](../../crates/lspf-hello/src/main.rs) with an end-to-end
stdio test beside it.

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

## Workspace and Documents ownership

The framework owns the connection's [`Workspace`] and [`Documents`]; user
state never holds either. Handlers reach both through the [`ServerContext`]
parameter: `ctx.workspace()` and `ctx.documents()` — the read-only
[`DocumentsView`], a view over the store only the protocol engine's built-in
document-sync handlers ever mutate.

`Workspace` carries the client's announcements verbatim — client info,
client capabilities, initialization options, the root URI, and the workspace
folders in announced order — and its later mutations come from the protocol:
`workspace/didChangeWorkspaceFolders`, `workspace/didChangeConfiguration`,
and `$/setTrace`. Clones are cheap handles onto one shared state, so any
handler sees the current connection state:

```rust
# use std::sync::Arc;
# use lspf::{CancellationToken, ServerContext, LspError, Server};
# struct State;
async fn roots(
    _state: Arc<State>,
    ctx: ServerContext,
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

Documents the framework tracks are synchronized before user code runs. A
registration for a built-in document notification — `textDocument/didOpen`,
`didChange`, `didClose`, `willSave`, `didSave` — records the connection's
one post-validation hook: the engine decodes and mutates first, and the hook
observes the result through `ctx.documents()`.

## Commands

A Command is a typed closure dispatched by name beneath
`workspace/executeCommand`. The engine decodes the command's `arguments`
array into `Args` (tuples, structs, and `Vec` alike; an absent `arguments`
decodes as an empty array) and returns `Output` as the command result:

```rust
# use std::sync::Arc;
# use lspf::{CancellationToken, ServerContext, LspError, Server};
# struct State;
async fn count_words(
    _state: Arc<State>,
    ctx: ServerContext,
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

Each registered name merges into one de-duplicated `executeCommandProvider`
whose `commands` list matches registration order exactly (ADR 0022), so the
advertised order never depends on hashing or on a later re-sort. Command
registration mistakes are static `BuildError`s too: an empty name
(`EmptyCommandName`), two handlers for one name (`DuplicateCommand`), or any
command beside an explicit `workspace/executeCommand` request handler
(`ExecuteCommandConflict`).

## FileProvider configuration

`ctx.workspace().text_document(uri)` resolves a document snapshot: editor-open
text first, then the connection's configured [`FileProvider`]. The provider
is owned by the connection's workspace, configured once on the builder, and
never caches — every lookup asks it again. Two implementations ship:

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

[`MemoryFileProvider`] serves virtual resources and tests; clones share one
backing store, and lookup uses the same normalized URI identity as open
documents:

```rust
# fn main() {
let provider = lspf::MemoryFileProvider::new();
provider.insert(
    "file:///virtual/notes.txt".parse::<lspf::types::Uri>().unwrap(),
    "virtual text",
);
# }
```

Failures surface through [`WorkspaceError`]: `NotFound` when the provider has
no resource, `UnsupportedScheme` for a scheme the provider does not serve,
`InvalidEncoding` for non-UTF-8 contents, `TooLarge` past the configured
limit, and `Io` for the underlying read error.

[`lspf::features`]: https://docs.rs/lspf/latest/lspf/features/
[`lspf::types::request`]: https://docs.rs/lspf/latest/lspf/types/request/
[`lspf::types::notification`]: https://docs.rs/lspf/latest/lspf/types/notification/
[`BuildError`]: https://docs.rs/lspf/latest/lspf/enum.BuildError.html
[`InitializeRegistrar`]: https://docs.rs/lspf/latest/lspf/struct.InitializeRegistrar.html
[`Workspace`]: https://docs.rs/lspf/latest/lspf/struct.Workspace.html
[`Documents`]: https://docs.rs/lspf/latest/lspf/struct.DocumentsView.html
[`DocumentsView`]: https://docs.rs/lspf/latest/lspf/struct.DocumentsView.html
[`ServerContext`]: https://docs.rs/lspf/latest/lspf/struct.ServerContext.html
[`FileProvider`]: https://docs.rs/lspf/latest/lspf/trait.FileProvider.html
[`MemoryFileProvider`]: https://docs.rs/lspf/latest/lspf/struct.MemoryFileProvider.html
[`WorkspaceError`]: https://docs.rs/lspf/latest/lspf/enum.WorkspaceError.html
