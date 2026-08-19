//! Business logic shared by every Transport example.
//!
//! This module deliberately contains the handlers and registrations, but no
//! Transport choice. Native and WASM hosts build exactly this `Server` and
//! differ only in how they serve it.

use std::sync::Arc;

use lspf::types::notification::DidOpenTextDocument;
use lspf::types::{
    CompletionItem, CompletionOptions, CompletionParams, CompletionResponse,
    DidOpenTextDocumentParams, Hover, HoverParams,
};
use lspf::{CancellationToken, Context, LspError, Server};

/// Application state used unchanged by every host example.
pub(crate) struct State {
    label: String,
}

impl State {
    fn new() -> Self {
        Self {
            label: "shared".to_string(),
        }
    }
}

/// The typed `textDocument/hover` handler.
async fn hover(
    state: Arc<State>,
    ctx: Context,
    _params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let _ = ctx.workspace().roots();
    Ok(Some(Hover {
        contents: lspf::types::HoverContents::Scalar(lspf::types::MarkedString::String(
            state.label.clone(),
        )),
        range: None,
    }))
}

/// The typed `textDocument/completion` handler.
async fn completion(
    _state: Arc<State>,
    _ctx: Context,
    _params: CompletionParams,
    _ct: CancellationToken,
) -> Result<Option<CompletionResponse>, LspError> {
    Ok(Some(CompletionResponse::Array(vec![CompletionItem {
        label: "shared".to_string(),
        ..CompletionItem::default()
    }])))
}

/// A typed custom request marker: same method, params, and result on every
/// target.
enum SharedPing {}

impl lspf::types::request::Request for SharedPing {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "shared/ping";
}

async fn ping(
    state: Arc<State>,
    _ctx: Context,
    params: String,
    _ct: CancellationToken,
) -> Result<String, LspError> {
    Ok(format!("{}:{params}", state.label))
}

/// The post-mutation hook for the built-in `textDocument/didOpen`.
async fn on_did_open(_state: Arc<State>, _ctx: Context, _params: DidOpenTextDocumentParams) {}

/// Build the one handler set used by stdio, TCP, WebSocket, and
/// worker-channel serving.
pub(crate) fn build() -> Result<Server<State>, lspf::BuildError> {
    Server::builder(State::new())
        .feature(lspf::features::hover(), hover)
        .feature(
            lspf::features::completion(CompletionOptions::default()),
            completion,
        )
        .request::<SharedPing, _, _>(ping)
        .notification::<DidOpenTextDocument, _, _>(on_did_open)
        .build()
}
