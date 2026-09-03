//! Business logic shared by every Transport example.
//!
//! This module deliberately contains the handlers and registrations, but no
//! Transport choice. Native and WASM hosts build exactly this `Server` and
//! differ only in how they serve it.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use lspf::types::notification::DidOpenTextDocument;
use lspf::types::{
    CompletionItem, CompletionOptions, CompletionParams, CompletionResponse,
    DidOpenTextDocumentParams, Hover, HoverParams, MarkupContent, MarkupKind,
};
use lspf::{CancellationToken, LspError, Server, ServerContext};

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
    ctx: ServerContext,
    _params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let _ = ctx.workspace().roots();
    Ok(Some(Hover {
        contents: lspf::types::HoverContents::MarkupContent(MarkupContent::new(
            MarkupKind::Markdown,
            state.label.clone(),
        )),
        range: None,
    }))
}

/// The typed `textDocument/completion` handler.
async fn completion(
    _state: Arc<State>,
    _ctx: ServerContext,
    _params: CompletionParams,
    _ct: CancellationToken,
) -> Result<Option<CompletionResponse>, LspError> {
    Ok(Some(CompletionResponse::CompletionItemList(vec![
        CompletionItem {
            label: "shared".to_string(),
            ..CompletionItem::default()
        },
    ])))
}

/// The parameters of the custom `shared/ping` request.
///
/// JSON-RPC 2.0 only carries `params` as an object or an array, so a custom
/// method takes an interface like every LSP method does. A bare `String` here
/// would make the request unsendable by a conforming client.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SharedPingParams {
    pub(crate) message: String,
}

/// The result of the custom `shared/ping` request.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SharedPingResult {
    pub(crate) reply: String,
}

/// A typed custom request marker: same method, params, and result on every
/// target.
enum SharedPing {}

impl lspf::types::request::Request for SharedPing {
    type Params = SharedPingParams;
    type Result = SharedPingResult;
    const METHOD: &'static str = "shared/ping";
}

async fn ping(
    state: Arc<State>,
    _ctx: ServerContext,
    params: SharedPingParams,
    _ct: CancellationToken,
) -> Result<SharedPingResult, LspError> {
    Ok(SharedPingResult {
        reply: format!("{}:{}", state.label, params.message),
    })
}

/// The post-mutation hook for the built-in `textDocument/didOpen`.
async fn on_did_open(_state: Arc<State>, _ctx: ServerContext, _params: DidOpenTextDocumentParams) {}

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
