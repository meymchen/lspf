use std::future::Future;

use lsp_types::{
    DidOpenTextDocumentParams, InitializeParams, InitializeResult, InitializedParams,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
};
use tokio_util::sync::CancellationToken;

use crate::context::Context;
use crate::documents::{Documents, PositionEncoding};
use crate::error::LspError;

/// Intersect the client's offered `positionEncodings` with lspf's preference
/// order (`utf-8` then `utf-16`), write the choice into the document store,
/// and return the LSP kind to advertise (ADR 0016).
///
/// If the client offers nothing, nothing supported, or omits the field
/// entirely, the encoding defaults to UTF-16.
fn negotiate_position_encoding(
    documents: &Documents,
    params: &InitializeParams,
) -> lsp_types::PositionEncodingKind {
    let offered = params
        .capabilities
        .general
        .as_ref()
        .and_then(|g| g.position_encodings.as_deref());

    let preferred = [
        lsp_types::PositionEncodingKind::UTF8,
        lsp_types::PositionEncodingKind::UTF16,
    ];
    let chosen = offered
        .and_then(|encodings| {
            preferred
                .iter()
                .find(|kind| encodings.contains(kind))
                .cloned()
        })
        .unwrap_or(lsp_types::PositionEncodingKind::UTF16);

    documents.set_position_encoding(if chosen == lsp_types::PositionEncodingKind::UTF8 {
        PositionEncoding::Utf8
    } else {
        PositionEncoding::Utf16
    });

    chosen
}

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

    /// The framework-provided document store (ADR 0003).
    ///
    /// The typical handler reads documents through `self.documents()`
    /// without writing lock code. The store is concurrency-safe and cheap
    /// to clone.
    fn documents(&self) -> &Documents;

    fn server_capabilities(&self, params: &InitializeParams) -> ServerCapabilities {
        let position_encoding = negotiate_position_encoding(self.documents(), params);
        ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Kind(Self::TEXT_DOCUMENT_SYNC)),
            position_encoding: Some(position_encoding),
            ..ServerCapabilities::default()
        }
    }

    fn initialize(
        &self,
        _ctx: &Context,
        params: InitializeParams,
        _ct: CancellationToken,
    ) -> impl Future<Output = Result<InitializeResult, LspError>> + Send {
        async move {
            Ok(InitializeResult {
                capabilities: self.server_capabilities(&params),
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
