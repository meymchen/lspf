//! Push-model diagnostics server.

mod example_support;

use std::sync::Arc;

use lspf::types::notification::{DidChangeTextDocument, DidOpenTextDocument};
use lspf::types::{
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, PublishDiagnosticsParams,
};
use lspf::{Server, ServerContext};

struct State;

async fn publish(ctx: ServerContext, uri: lspf::types::Uri) {
    let Some(document) = ctx.documents().get(&uri) else {
        return;
    };
    let _ = ctx.publish_diagnostics(PublishDiagnosticsParams {
        uri,
        diagnostics: example_support::sum_diagnostics(&document.text()),
        version: document.version(),
    });
}

async fn did_open(_: Arc<State>, ctx: ServerContext, params: DidOpenTextDocumentParams) {
    publish(ctx, params.text_document.uri).await;
}

async fn did_change(_: Arc<State>, ctx: ServerContext, params: DidChangeTextDocumentParams) {
    publish(ctx, params.text_document.uri).await;
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .notification::<DidOpenTextDocument, _, _>(did_open)
        .notification::<DidChangeTextDocument, _, _>(did_change)
        .build()
        .expect("diagnostic hooks are valid");
    example_support::serve(server).await
}
