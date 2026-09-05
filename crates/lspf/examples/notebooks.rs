//! Notebook lifecycle hooks and hover over synchronized cell Documents.

mod example_support;

use std::sync::Arc;

use lspf::types::notification::{
    DidChangeNotebookDocument, DidCloseNotebookDocument, DidOpenNotebookDocument,
    DidSaveNotebookDocument,
};
use lspf::types::{
    DidChangeNotebookDocumentParams, DidCloseNotebookDocumentParams, DidOpenNotebookDocumentParams,
    DidSaveNotebookDocumentParams, Hover, HoverContents, HoverParams, LogMessageParams,
    MarkupContent, MarkupKind, MessageType, NotebookDocumentFilterWithNotebook,
    NotebookDocumentSyncOptions, Uri,
};
use lspf::{CancellationToken, LspError, Server, ServerContext};

// Hooks read framework-owned state after synchronization. The application
// needs neither its own Notebook store nor a second copy of cell text.
fn log_notebook(ctx: &ServerContext, uri: &Uri, event: &str) {
    let message = match ctx.notebooks().get(uri) {
        Some(notebook) => format!(
            "{event}: {} (version {}, {} cells)",
            notebook.uri(),
            notebook.version(),
            notebook.cells().len(),
        ),
        None => format!("{event}: {uri} is closed"),
    };
    if let Err(error) = ctx.client().log_message(LogMessageParams {
        kind: MessageType::Info,
        message,
    }) {
        tracing::warn!(%error, "could not enqueue notebook lifecycle log");
    }
}

async fn did_open(_: Arc<()>, ctx: ServerContext, params: DidOpenNotebookDocumentParams) {
    log_notebook(&ctx, &params.notebook_document.uri, "open");
}

async fn did_change(_: Arc<()>, ctx: ServerContext, params: DidChangeNotebookDocumentParams) {
    log_notebook(&ctx, &params.notebook_document.uri, "change");
}

async fn did_save(_: Arc<()>, ctx: ServerContext, params: DidSaveNotebookDocumentParams) {
    log_notebook(&ctx, &params.notebook_document.uri, "save");
}

async fn did_close(_: Arc<()>, ctx: ServerContext, params: DidCloseNotebookDocumentParams) {
    log_notebook(&ctx, &params.notebook_document.uri, "close");
}

async fn hover(
    _: Arc<()>,
    ctx: ServerContext,
    params: HoverParams,
    _: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let Some(notebook) = ctx.notebooks().notebook_for_cell(&uri) else {
        return Ok(None);
    };
    let Some(document) = ctx.documents().get(&uri) else {
        return Ok(None);
    };
    Ok(Some(Hover {
        contents: HoverContents::MarkupContent(MarkupContent {
            kind: MarkupKind::PlainText,
            value: format!(
                "Notebook: {}\nNotebook version: {}\nCells: {}\nCell version: {}\n\n{}",
                notebook.uri(),
                notebook.version(),
                notebook.cells().len(),
                document.version().unwrap_or_default(),
                document.text(),
            ),
        }),
        range: None,
    }))
}

fn server() -> Server<()> {
    Server::builder(())
        .notebook_document_sync(NotebookDocumentSyncOptions::new(
            vec![NotebookDocumentFilterWithNotebook::new("jupyter-notebook".into(), None).into()],
            Some(true),
        ))
        .notification::<DidOpenNotebookDocument, _, _>(did_open)
        .notification::<DidChangeNotebookDocument, _, _>(did_change)
        .notification::<DidSaveNotebookDocument, _, _>(did_save)
        .notification::<DidCloseNotebookDocument, _, _>(did_close)
        .feature(lspf::features::hover(), hover)
        .build()
        .expect("notebook hooks and hover are valid")
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    example_support::serve(server()).await
}

#[cfg(all(test, feature = "testing"))]
#[path = "notebooks/tests.rs"]
mod tests;
