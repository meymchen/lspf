use lsp_types::PublishDiagnosticsParams;
use lsp_types::notification::PublishDiagnostics;
use tokio_util::sync::CancellationToken;
use tracing::{Span, warn};

use crate::client::{Client, OutboundRegistry};
use crate::documents::Documents;
use crate::raw::RequestId;
use crate::workspace::Workspace;

/// Per-request handle to framework state (see ADR 0009).
///
/// The handle exposes connection-scoped capabilities such as [`Client`],
/// [`Documents`], and [`Workspace`] without exposing protocol-owned queues or
/// registries.
#[derive(Debug, Clone)]
pub struct Context {
    pub(crate) request_id: Option<RequestId>,
    pub(crate) span: Span,
    pub(crate) client: Client,
    pub(crate) documents: Documents,
    /// The connection's established [`Workspace`], present once the initialize
    /// transaction has run. Handlers only run after that point, so a handler
    /// always observes `Some`; it is `None` only in the pre-init lifecycle
    /// hooks and legacy dispatch paths that predate workspace establishment.
    pub(crate) workspace: Option<Workspace>,
    pub(crate) cancellation: Option<CancellationToken>,
}

impl Context {
    pub(crate) fn for_request(
        id: RequestId,
        span: Span,
        client: Client,
        documents: Documents,
    ) -> Self {
        Self {
            request_id: Some(id),
            span,
            client,
            documents,
            workspace: None,
            cancellation: None,
        }
    }

    pub(crate) fn for_notification(span: Span, client: Client, documents: Documents) -> Self {
        Self {
            request_id: None,
            span,
            client,
            documents,
            workspace: None,
            cancellation: None,
        }
    }

    /// Attach the connection's established [`Workspace`] to this context. The
    /// protocol engine calls this once the initialize transaction has run, so
    /// every handler and lifecycle hook that observes a workspace sees the same
    /// established state.
    pub(crate) fn with_workspace(mut self, workspace: Workspace) -> Self {
        self.workspace = Some(workspace);
        self
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

    /// The framework document store.
    pub fn documents(&self) -> &Documents {
        &self.documents
    }

    /// A cheap clone of the typed handle for this connection's LSP client.
    pub fn client(&self) -> Client {
        self.client.clone()
    }

    /// The connection's [`Workspace`], established from `InitializeParams`
    /// during the initialize transaction (ADR 0017, ADR 0018).
    ///
    /// Returns `None` only where no workspace has been established: the
    /// `on_initialize`-and-later handlers of the 0.2 engine always see `Some`,
    /// while the 0.1 `LanguageServer` dispatch path and the test-only
    /// constructor carry none.
    pub fn workspace(&self) -> Option<&Workspace> {
        self.workspace.as_ref()
    }

    #[doc(hidden)]
    /// Test-only constructor that builds a notification context with a
    /// dummy outgoing channel and a placeholder span.
    pub fn for_test_notification(documents: Documents) -> Self {
        let (outgoing, _rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            request_id: None,
            span: Span::current(),
            client: Client::new(outgoing, OutboundRegistry::default()),
            documents,
            workspace: None,
            cancellation: None,
        }
    }

    /// Push a `textDocument/publishDiagnostics` notification through the
    /// connection's typed [`Client`] (fire-and-forget).
    ///
    /// Errors during serialization or send (channel closed during
    /// shutdown) are logged via `tracing::warn!` rather than surfaced —
    /// the LSP semantics of `publishDiagnostics` is "best effort"; a
    /// failed publish never invalidates the handler that triggered it.
    pub fn publish_diagnostics(&self, params: PublishDiagnosticsParams) {
        if let Err(error) = self.client.notify::<PublishDiagnostics>(params) {
            warn!(%error, "publish_diagnostics: notification failed");
        }
    }
}
