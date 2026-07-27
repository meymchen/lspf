use std::borrow::Cow;

use bytes::Bytes;
use lsp_types::PublishDiagnosticsParams;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{Span, warn};

use crate::documents::Documents;
use crate::raw::{RawMessage, RequestId};
use crate::workspace::Workspace;

/// Per-request handle to framework state (see ADR 0009).
///
/// Commit 1 carried only the request scope; commit 2 adds the send-side
/// channel through which outgoing helpers (`publish_diagnostics`,
/// `show_message`, `apply_edit`, …) push notifications and requests onto
/// the wire. The `Documents` store and workspace-folder cache are
/// added field-by-field as later commits implement them — `Context`
/// grows by accretion, never holding `todo!()` stubs.
#[derive(Debug, Clone)]
pub struct Context {
    pub(crate) request_id: Option<RequestId>,
    pub(crate) span: Span,
    pub(crate) outgoing: UnboundedSender<RawMessage>,
    pub(crate) documents: Documents,
    /// The connection's established [`Workspace`], present once the initialize
    /// transaction has run. Handlers only run after that point, so a handler
    /// always observes `Some`; it is `None` only in the pre-init lifecycle
    /// hooks and legacy dispatch paths that predate workspace establishment.
    pub(crate) workspace: Option<Workspace>,
}

impl Context {
    pub(crate) fn for_request(
        id: RequestId,
        span: Span,
        outgoing: UnboundedSender<RawMessage>,
        documents: Documents,
    ) -> Self {
        Self {
            request_id: Some(id),
            span,
            outgoing,
            documents,
            workspace: None,
        }
    }

    pub(crate) fn for_notification(
        span: Span,
        outgoing: UnboundedSender<RawMessage>,
        documents: Documents,
    ) -> Self {
        Self {
            request_id: None,
            span,
            outgoing,
            documents,
            workspace: None,
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
            outgoing,
            documents,
            workspace: None,
        }
    }

    /// Push a `textDocument/publishDiagnostics` notification onto the
    /// outgoing channel (fire-and-forget). The dispatcher drains the
    /// channel into the transport between handler invocations.
    ///
    /// Errors during serialization or send (channel closed during
    /// shutdown) are logged via `tracing::warn!` rather than surfaced —
    /// the LSP semantics of `publishDiagnostics` is "best effort"; a
    /// failed publish never invalidates the handler that triggered it.
    pub fn publish_diagnostics(&self, params: PublishDiagnosticsParams) {
        let body = match serde_json::to_vec(&params) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "publish_diagnostics: serialize failed");
                return;
            }
        };
        let msg = RawMessage::Notification {
            method: Cow::Borrowed("textDocument/publishDiagnostics"),
            params: Bytes::from(body),
        };
        if self.outgoing.send(msg).is_err() {
            warn!("publish_diagnostics: outgoing channel closed");
        }
    }
}
