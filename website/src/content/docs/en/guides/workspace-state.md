---
title: Manage workspace state
description: Work with synchronized documents, notebooks, commands, and unopened files.
---

A server’s workspace state is connection-scoped and available through `ServerContext`. This guide covers the stateful half of server implementation.

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

## Notebook synchronization

All four `notebookDocument/*` notifications are protocol built-ins: the engine
decodes each one and mutates the connection's notebook and document state
itself, and that behaviour cannot be replaced. Registering one of these methods
records a post-mutation hook, exactly as a text-document registration does, so
there is no notebook *handler* to write (ADR 0034).

Notebook sync is opt-in, and
[`notebook_document_sync(options)`](lspf::ServerBuilder::notebook_document_sync)
is the opt-in. It advertises `notebookDocumentSync`, which is what makes a
client send notebook notifications at all, and it is also what makes the four
built-ins reachable: a server that never calls it ignores a notebook
notification that arrives anyway, mutating nothing and running no hook. Notebook
sync is its own LSP capability rather than a mode of `textDocumentSync`, so the
text-document sync switches neither enable nor disable it.

```rust
# use lspf::types::{NotebookDocumentFilterWithNotebook, NotebookDocumentSyncOptions};
# use lspf::Server;
# struct State;
# fn main() {
let server = Server::builder(State)
    .notebook_document_sync(NotebookDocumentSyncOptions::new(
        // Sync Jupyter notebooks whatever their cells contain.
        vec![NotebookDocumentFilterWithNotebook::new("jupyter-notebook".into(), None).into()],
        // Ask the client to forward `notebookDocument/didSave`.
        Some(true),
    ))
    .build()
    .expect("the static registrations are valid");
# }
```

The framework splits a notebook across two stores. [`NotebooksView`] — reached
through `ctx.notebooks()` — holds notebook type, version, metadata, and ordered
cell membership. Cell *text* is not there: every cell is an ordinary
[`Document`] under its own cell URI, so it reads through the same
`ctx.documents()` view, with the same rope, incremental change path, and
position encoding as any other document.

```rust
# use std::sync::Arc;
# use lspf::types::Uri;
# use lspf::{CancellationToken, ServerContext, LspError, Server};
# struct State;
/// Concatenate a notebook's cells in document order.
async fn notebook_source(
    _state: Arc<State>,
    ctx: ServerContext,
    args: Vec<String>,
    _ct: CancellationToken,
) -> Result<String, LspError> {
    let uri: Uri = args
        .first()
        .ok_or_else(|| LspError::invalid_params("expected a notebook URI"))?
        .parse()
        .map_err(LspError::invalid_params)?;
    let Some(notebook) = ctx.notebooks().get(&uri) else {
        return Ok(String::new());
    };
    let documents = ctx.documents();
    Ok(notebook
        .cells()
        .iter()
        // Membership and order come from the notebook view; text comes from
        // the document store.
        .filter_map(|cell| documents.get(&cell.document))
        .map(|document| document.text())
        .collect::<Vec<_>>()
        .join("\n"))
}
# fn main() {
#     let server = Server::builder(State)
#         .command("example.notebookSource", notebook_source)
#         .build()
#         .expect("the static registrations are valid");
# }
```

[`NotebooksView::notebook_for_cell`](lspf::NotebooksView::notebook_for_cell)
walks the other way, from a cell URI to the notebook holding it — the lookup a
`textDocument/*` handler needs when the client sends it a cell URI.

Two consequences are worth stating outright:

- **Notebook notifications never synthesize text-document ones.** The notebook
  hook is the *only* hook a notebook notification runs. A cell edit inside
  `notebookDocument/didChange` does not
  invoke the `textDocument/didChange` hook, and opening or closing a notebook
  does not invoke the open or close hooks for its cell Documents.
- **Cells are metered as documents.** Every cell counts toward
  `ResourcePolicy::max_documents` and its text toward `max_document_bytes`;
  the separate `max_notebooks` budget bounds notebook-level state so an empty
  notebook still costs something finite. An open that would exceed any of the
  three is refused before mutation, leaving neither the notebook nor any of
  its cell Documents behind.

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

[`Workspace`]: https://docs.rs/lspf/latest/lspf/struct.Workspace.html
[`Documents`]: https://docs.rs/lspf/latest/lspf/struct.DocumentsView.html
[`DocumentsView`]: https://docs.rs/lspf/latest/lspf/struct.DocumentsView.html
[`Document`]: https://docs.rs/lspf/latest/lspf/struct.Document.html
[`NotebooksView`]: https://docs.rs/lspf/latest/lspf/struct.NotebooksView.html
[`ServerContext`]: https://docs.rs/lspf/latest/lspf/struct.ServerContext.html
[`FileProvider`]: https://docs.rs/lspf/latest/lspf/trait.FileProvider.html
[`MemoryFileProvider`]: https://docs.rs/lspf/latest/lspf/struct.MemoryFileProvider.html
[`WorkspaceError`]: https://docs.rs/lspf/latest/lspf/enum.WorkspaceError.html
