use std::borrow::Cow;

use bytes::Bytes;
use lsp_types::PublishDiagnosticsParams;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{Span, warn};

use crate::raw::{RawMessage, RequestId};

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
}

impl Context {
    pub(crate) fn for_request(
        id: RequestId,
        span: Span,
        outgoing: UnboundedSender<RawMessage>,
    ) -> Self {
        Self {
            request_id: Some(id),
            span,
            outgoing,
        }
    }

    pub(crate) fn for_notification(span: Span, outgoing: UnboundedSender<RawMessage>) -> Self {
        Self {
            request_id: None,
            span,
            outgoing,
        }
    }

    pub fn request_id(&self) -> Option<&RequestId> {
        self.request_id.as_ref()
    }

    pub fn span(&self) -> &Span {
        &self.span
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
