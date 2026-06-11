use tracing::Span;

use crate::raw::RequestId;

/// Per-request handle to framework state (see ADR 0009).
///
/// Commit 1 only carries the request scope (id + tracing span). The
/// `Documents` store, workspace-folder cache, and outgoing helpers
/// (`publish_diagnostics`, `apply_edit`, …) are added field-by-field as
/// later commits implement them — `Context` grows by accretion, never
/// holding `todo!()` stubs.
#[derive(Debug, Clone)]
pub struct Context {
    pub(crate) request_id: Option<RequestId>,
    pub(crate) span: Span,
}

impl Context {
    pub(crate) fn for_request(id: RequestId, span: Span) -> Self {
        Self {
            request_id: Some(id),
            span,
        }
    }

    pub(crate) fn for_notification(span: Span) -> Self {
        Self {
            request_id: None,
            span,
        }
    }

    pub fn request_id(&self) -> Option<&RequestId> {
        self.request_id.as_ref()
    }

    pub fn span(&self) -> &Span {
        &self.span
    }
}
