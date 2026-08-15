use lsp_types::PublishDiagnosticsParams;
use tokio_util::sync::CancellationToken;
use tracing::Span;

use crate::client::Client;
use crate::documents::DocumentsView;
use crate::error::ClientError;
use crate::raw::RequestId;
use crate::workspace::Workspace;

/// Per-request handle to framework state (see ADR 0009).
///
/// The handle exposes connection-scoped capabilities — the established
/// [`Workspace`] (initialization metadata, roots, workspace folders, and the
/// read-only [`DocumentsView`]) and the [`Client`] — without exposing
/// protocol-owned queues or registries.
#[derive(Debug, Clone)]
pub struct Context {
    pub(crate) request_id: Option<RequestId>,
    pub(crate) span: Span,
    pub(crate) client: Client,
    /// The connection's established [`Workspace`]. Handlers only run after
    /// the initialize transaction has established it, so user code always
    /// observes one — there is no workspace-less dispatch.
    pub(crate) workspace: Workspace,
    pub(crate) cancellation: Option<CancellationToken>,
}

impl Context {
    pub(crate) fn for_request(
        id: RequestId,
        span: Span,
        client: Client,
        workspace: Workspace,
    ) -> Self {
        Self {
            request_id: Some(id),
            span,
            client,
            workspace,
            cancellation: None,
        }
    }

    pub(crate) fn for_notification(span: Span, client: Client, workspace: Workspace) -> Self {
        Self {
            request_id: None,
            span,
            client,
            workspace,
            cancellation: None,
        }
    }

    pub(crate) fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    pub(crate) fn cancellation(&self) -> Option<&CancellationToken> {
        self.cancellation.as_ref()
    }

    pub fn request_id(&self) -> Option<&RequestId> {
        self.request_id.as_ref()
    }

    pub fn span(&self) -> &Span {
        &self.span
    }

    /// The connection's documents, as a read-only [`DocumentsView`] — the
    /// same view [`Workspace::documents`] hands out.
    ///
    /// The framework owns and mutates the documents; a handler reads the
    /// retained documents and converts positions through this view, and a
    /// registered document hook sees it already carrying the built-in
    /// mutation (ADR 0018).
    pub fn documents(&self) -> DocumentsView {
        self.workspace.documents()
    }

    /// A cheap clone of the typed handle for this connection's LSP client.
    pub fn client(&self) -> Client {
        self.client.clone()
    }

    /// The connection's [`Workspace`], established from `InitializeParams`
    /// during the initialize transaction (ADR 0017, ADR 0018): the one
    /// cheap, shared, read-only view of the initialization metadata, roots,
    /// workspace folders, and documents every handler observes.
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Push a `textDocument/publishDiagnostics` notification through the
    /// connection's typed [`Client`] (fire-and-forget); see
    /// [`Client::publish_diagnostics`] for the exact semantics.
    ///
    /// Serialization and enqueue failures are returned to the caller (the
    /// client helper also reports them through `tracing`). A failed publish
    /// never invalidates the handler that triggered it, so a handler that
    /// treats publishing as best effort may simply drop the error.
    pub fn publish_diagnostics(&self, params: PublishDiagnosticsParams) -> Result<(), ClientError> {
        self.client.publish_diagnostics(params)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use lsp_types::{InitializeParams, TextDocumentItem, Uri};

    use super::*;
    use crate::client::{OutboundQueue, OutboundRegistry};
    use crate::documents::Documents;

    fn context() -> (Context, Documents) {
        let (out_tx, _out_rx) = OutboundQueue::new(crate::DEFAULT_OUTBOUND_WARNING_THRESHOLD);
        let documents = Documents::new();
        let workspace = Workspace::from_params(&InitializeParams::default(), documents.clone());
        let client = Client::new(out_tx, OutboundRegistry::default());
        (
            Context::for_notification(Span::none(), client, workspace),
            documents,
        )
    }

    #[test]
    fn cloning_shares_connection_state_instead_of_copying_it() {
        let (ctx, documents) = context();
        let clone = ctx.clone();

        let uri = Uri::from_str("file:///shared.rs").unwrap();
        documents.open(TextDocumentItem {
            uri: uri.clone(),
            language_id: "rust".to_string(),
            version: 1,
            text: "fn main() {}".to_string(),
        });

        let doc = clone
            .documents()
            .get(&uri)
            .expect("a cloned context reads the same connection documents");
        assert_eq!(doc.text(), "fn main() {}");
        assert_eq!(
            clone.workspace().roots(),
            ctx.workspace().roots(),
            "a cloned context shares the one workspace"
        );
    }
}
