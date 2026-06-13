use std::future::Future;

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
/// Method futures are explicitly `Send` so the dispatcher can spawn each
/// non-lifecycle handler on its own `tokio::task` (ADR 0003 addendum).
/// User impls keep writing `async fn` overrides — the compiler verifies
/// the body is `Send`.
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

    fn initialize(
        &self,
        _ctx: &Context,
        _params: InitializeParams,
        _ct: CancellationToken,
    ) -> impl Future<Output = Result<InitializeResult, LspError>> + Send {
        async {
            Ok(InitializeResult {
                capabilities: self.server_capabilities(),
                server_info: None,
            })
        }
    }

    fn initialized(
        &self,
        _ctx: &Context,
        _params: InitializedParams,
    ) -> impl Future<Output = ()> + Send {
        async {}
    }

    fn shutdown(
        &self,
        _ctx: &Context,
        _ct: CancellationToken,
    ) -> impl Future<Output = Result<(), LspError>> + Send {
        async { Ok(()) }
    }

    fn exit(&self, _ctx: &Context) -> impl Future<Output = ()> + Send {
        async {}
    }

    fn text_document_did_open(
        &self,
        _ctx: &Context,
        _params: DidOpenTextDocumentParams,
    ) -> impl Future<Output = ()> + Send {
        async {}
    }
}
