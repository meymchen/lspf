// Commit 1's dispatcher is sequential (ADR 0010's Layer/Service generalization
// lands in commit 2+), so trait methods don't need `+ Send` on their returned
// futures yet. We keep `async fn` syntax for user ergonomics and revisit Send
// bounds when spawning is introduced.
#![allow(async_fn_in_trait)]

use lsp_types::{
    DidOpenTextDocumentParams, InitializeParams, InitializeResult, InitializedParams,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
};
use tokio_util::sync::CancellationToken;

use crate::context::Context;
use crate::error::LspError;

/// The user's language server (see ADR 0003, 0004, 0006, 0007, 0009).
///
/// Methods mirror LSP wire names (`textDocument/hover` →
/// `text_document_hover`). Every request handler takes `(ctx, params,
/// ct)`; every notification handler takes `(ctx, params)`. Returning
/// `Err(LspError)` from a request handler sends a JSON-RPC error response
/// to the client (see ADR 0006); panics are caught by the framework's
/// default panic layer (commit 2+).
///
/// Capabilities are auto-derived from the trait's associated consts
/// (ADR 0004). Commit 1 wires only `TEXT_DOCUMENT_SYNC`; subsequent
/// commits add one const per LSP feature.
pub trait LanguageServer: Send + Sync + 'static {
    const TEXT_DOCUMENT_SYNC: TextDocumentSyncKind = TextDocumentSyncKind::INCREMENTAL;

    fn server_capabilities(&self) -> ServerCapabilities {
        ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Kind(Self::TEXT_DOCUMENT_SYNC)),
            ..ServerCapabilities::default()
        }
    }

    async fn initialize(
        &self,
        _ctx: &Context,
        _params: InitializeParams,
        _ct: CancellationToken,
    ) -> Result<InitializeResult, LspError> {
        Ok(InitializeResult {
            capabilities: self.server_capabilities(),
            server_info: None,
        })
    }

    async fn initialized(&self, _ctx: &Context, _params: InitializedParams) {}

    async fn shutdown(&self, _ctx: &Context, _ct: CancellationToken) -> Result<(), LspError> {
        Ok(())
    }

    async fn exit(&self, _ctx: &Context) {}

    async fn text_document_did_open(&self, _ctx: &Context, _params: DidOpenTextDocumentParams) {}
}
