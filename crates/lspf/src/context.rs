use gen_lsp_types::{ProgressToken, PublishDiagnosticsParams};
use tokio_util::sync::CancellationToken;
use tracing::Span;

use crate::client::ClientHandle;
use crate::documents::DocumentsView;
use crate::error::ClientError;
use crate::notebooks::NotebooksView;
use crate::partial_result::{PartialResultRequest, PartialResultScope, PartialResultSink};
use crate::progress::{ProgressHandle, ProgressOptions};
use crate::raw::RequestId;
use crate::workspace::Workspace;

/// Per-request handle to framework state (see ADR 0009).
///
/// The handle exposes connection-scoped capabilities — the established
/// [`Workspace`] (initialization metadata, roots, workspace folders, and its
/// read-only document and notebook views) and the [`ClientHandle`] without
/// exposing protocol-owned queues or registries.
#[derive(Debug, Clone)]
pub struct ServerContext {
    pub(crate) request_id: Option<RequestId>,
    pub(crate) span: Span,
    pub(crate) client: ClientHandle,
    /// The connection's established [`Workspace`]. Handlers only run after
    /// the initialize transaction has established it, so user code always
    /// observes one — there is no workspace-less dispatch.
    pub(crate) workspace: Workspace,
    pub(crate) cancellation: Option<CancellationToken>,
    pub(crate) work_done_token: Option<ProgressToken>,
    pub(crate) partial_result_scope: Option<PartialResultScope>,
}

impl ServerContext {
    pub(crate) fn for_request(
        id: RequestId,
        span: Span,
        client: ClientHandle,
        workspace: Workspace,
    ) -> Self {
        Self {
            request_id: Some(id),
            span,
            client,
            workspace,
            cancellation: None,
            work_done_token: None,
            partial_result_scope: None,
        }
    }

    pub(crate) fn for_notification(span: Span, client: ClientHandle, workspace: Workspace) -> Self {
        Self {
            request_id: None,
            span,
            client,
            workspace,
            cancellation: None,
            work_done_token: None,
            partial_result_scope: None,
        }
    }

    pub(crate) fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    pub(crate) fn with_work_done_token(mut self, token: Option<ProgressToken>) -> Self {
        self.work_done_token = token;
        self
    }

    pub(crate) fn with_partial_result(
        mut self,
        method: String,
        token: Option<ProgressToken>,
    ) -> Self {
        self.partial_result_scope = token.map(|token| PartialResultScope::new(method, token));
        self
    }

    pub(crate) fn partial_result_scope(&self) -> Option<PartialResultScope> {
        self.partial_result_scope.clone()
    }

    pub(crate) fn cancellation(&self) -> Option<&CancellationToken> {
        self.cancellation.as_ref()
    }

    /// The JSON-RPC request ID, or `None` while handling a notification.
    pub fn request_id(&self) -> Option<&RequestId> {
        self.request_id.as_ref()
    }

    /// The tracing span associated with the incoming call.
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

    /// The connection's notebooks, as a read-only [`NotebooksView`].
    pub fn notebooks(&self) -> NotebooksView {
        self.workspace.notebooks()
    }

    /// A cheap clone of the typed handle for this connection's LSP client.
    pub fn client(&self) -> ClientHandle {
        self.client.clone()
    }

    /// Begin progress on the token supplied with this inbound request.
    ///
    /// Returns `Ok(None)` when the request carried no `workDoneToken`. When a
    /// token is present, this sends the begin notification directly on that
    /// token without a `window/workDoneProgress/create` round trip.
    pub fn begin_progress(
        &self,
        options: ProgressOptions,
    ) -> Result<Option<ProgressHandle>, ClientError> {
        self.work_done_token
            .clone()
            .map(|token| self.client.begin_progress_with_token(token, options))
            .transpose()
    }

    /// Return this request's typed partial-result sink when the client supplied
    /// a `partialResultToken` and `R` is the request currently being handled.
    ///
    /// Only request markers generated from partial-result entries in the
    /// vendored LSP metaModel implement [`PartialResultRequest`]. Notifications,
    /// custom requests, unsupported standard methods, and requests without a
    /// token therefore cannot obtain a sink.
    pub fn partial_results<R>(&self) -> Option<PartialResultSink<'_, R>>
    where
        R: PartialResultRequest,
    {
        let scope = self.partial_result_scope.as_ref()?;
        if scope.method() != <R as crate::types::request::Request>::METHOD {
            return None;
        }
        Some(PartialResultSink::new(&self.client, scope))
    }

    /// The connection's [`Workspace`], established from `InitializeParams`
    /// during the initialize transaction (ADR 0017, ADR 0018): the one
    /// cheap, shared, read-only view of the initialization metadata, roots,
    /// workspace folders, and documents every handler observes.
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Push a `textDocument/publishDiagnostics` notification through the
    /// connection's typed [`ClientHandle`] (fire-and-forget); see
    /// [`ClientHandle::publish_diagnostics`] for the exact semantics.
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

    use futures_channel::mpsc::UnboundedReceiver;
    use gen_lsp_types::{
        DocumentSymbolPartialResponse, DocumentSymbolRequest, InitializeParams, NotebookCell,
        NotebookCellKind, NotebookDocument, ProgressToken, TextDocumentItem, Uri,
    };

    use super::*;
    use crate::client::{OutboundQueue, OutboundRegistry};
    use crate::documents::Documents;
    use crate::notebooks::Notebooks;
    use crate::raw::RawMessage;

    fn context() -> (ServerContext, Documents) {
        let (out_tx, _out_rx) = OutboundQueue::bounded(usize::MAX, usize::MAX);
        let documents = Documents::new();
        let workspace = Workspace::from_params(&InitializeParams::default(), documents.clone());
        let client = ClientHandle::new(out_tx, OutboundRegistry::default(), None);
        (
            ServerContext::for_notification(Span::none(), client, workspace),
            documents,
        )
    }

    fn context_with_notebooks() -> (ServerContext, Documents, Notebooks) {
        let (out_tx, _out_rx) = OutboundQueue::bounded(usize::MAX, usize::MAX);
        let documents = Documents::new();
        let notebooks = Notebooks::new();
        let workspace = Workspace::from_params_with_notebooks(
            &InitializeParams::default(),
            documents.clone(),
            notebooks.clone(),
        );
        let client = ClientHandle::new(out_tx, OutboundRegistry::default(), None);
        (
            ServerContext::for_notification(Span::none(), client, workspace),
            documents,
            notebooks,
        )
    }

    fn partial_result_context(
        max_outbound_messages: usize,
    ) -> (ServerContext, UnboundedReceiver<RawMessage>) {
        let (out_tx, out_rx) = OutboundQueue::bounded(max_outbound_messages, usize::MAX);
        let documents = Documents::new();
        let workspace = Workspace::from_params(&InitializeParams::default(), documents);
        let client = ClientHandle::new(out_tx, OutboundRegistry::default(), None);
        let ctx = ServerContext::for_request(RequestId::Number(7), Span::none(), client, workspace)
            .with_partial_result(
                <DocumentSymbolRequest as crate::types::request::Request>::METHOD.to_string(),
                Some(ProgressToken::String("symbols".into())),
            );
        (ctx, out_rx)
    }

    #[test]
    fn cloning_shares_connection_state_instead_of_copying_it() {
        let (ctx, documents) = context();
        let clone = ctx.clone();

        let uri = Uri::from_str("file:///shared.rs").unwrap();
        documents
            .open(TextDocumentItem {
                uri: uri.clone(),
                language_id: "rust".into(),
                version: 1,
                text: "fn main() {}".to_string(),
            })
            .expect("the default policy accepts the test document");

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

    #[test]
    fn notebooks_view_maps_a_notebook_to_its_cells_in_document_order() {
        let (ctx, _documents, notebooks) = context_with_notebooks();
        let notebook_uri = Uri::from_str("file:///analysis.ipynb").unwrap();
        let first_cell_uri = Uri::from_str("file:///analysis.ipynb#cell-1").unwrap();
        let second_cell_uri = Uri::from_str("file:///analysis.ipynb#cell-2").unwrap();
        notebooks.open(NotebookDocument::new(
            notebook_uri.clone(),
            "jupyter-notebook".into(),
            3,
            None,
            vec![
                NotebookCell::new(NotebookCellKind::Markup, first_cell_uri.clone(), None, None),
                NotebookCell::new(NotebookCellKind::Code, second_cell_uri.clone(), None, None),
            ],
        ));
        let notebook = ctx
            .notebooks()
            .get(&notebook_uri)
            .expect("the view resolves the opened notebook");

        assert_eq!(notebook.uri(), &notebook_uri);
        assert_eq!(notebook.notebook_type(), "jupyter-notebook");
        assert_eq!(notebook.version(), 3);
        assert_eq!(notebook.metadata(), None);
        assert_eq!(
            notebook
                .cells()
                .iter()
                .map(|cell| &cell.document)
                .collect::<Vec<_>>(),
            vec![&first_cell_uri, &second_cell_uri]
        );
    }

    #[test]
    fn notebooks_view_maps_a_cell_to_its_notebook_and_rejects_an_unknown_cell() {
        let (ctx, _documents, notebooks) = context_with_notebooks();
        let notebook_uri = Uri::from_str("file:///analysis.ipynb").unwrap();
        let cell_uri = Uri::from_str("file:///analysis.ipynb#cell-1").unwrap();
        let unknown_uri = Uri::from_str("file:///other.ipynb#cell-1").unwrap();
        notebooks.open(NotebookDocument::new(
            notebook_uri.clone(),
            "jupyter-notebook".into(),
            3,
            None,
            vec![NotebookCell::new(
                NotebookCellKind::Code,
                cell_uri.clone(),
                None,
                None,
            )],
        ));
        assert_eq!(
            ctx.notebooks()
                .notebook_for_cell(&cell_uri)
                .expect("the synchronized cell has an owning notebook")
                .uri(),
            &notebook_uri
        );
        assert!(ctx.notebooks().notebook_for_cell(&unknown_uri).is_none());
    }

    #[test]
    fn notebook_cell_text_is_read_through_the_documents_view() {
        let (ctx, documents, notebooks) = context_with_notebooks();
        let notebook_uri = Uri::from_str("file:///analysis.ipynb").unwrap();
        let cell_uri = Uri::from_str("file:///analysis.ipynb#cell-1").unwrap();
        notebooks.open(NotebookDocument::new(
            notebook_uri.clone(),
            "jupyter-notebook".into(),
            3,
            None,
            vec![NotebookCell::new(
                NotebookCellKind::Code,
                cell_uri.clone(),
                None,
                None,
            )],
        ));
        documents
            .open(TextDocumentItem {
                uri: cell_uri.clone(),
                language_id: "python".into(),
                version: 7,
                text: "print('one text engine')".into(),
            })
            .expect("the default policy accepts the cell document");
        let notebook = ctx
            .notebooks()
            .get(&notebook_uri)
            .expect("the notebook is synchronized");
        let cell = &notebook.cells()[0];
        assert_eq!(
            ctx.documents()
                .get(&cell.document)
                .expect("the cell text is an ordinary document")
                .text(),
            "print('one text engine')"
        );
    }

    #[test]
    fn partial_result_sink_requires_the_requests_token() {
        let (ctx, _) = context();
        let ctx = ctx.with_partial_result(
            <DocumentSymbolRequest as crate::types::request::Request>::METHOD.to_string(),
            None,
        );

        assert!(ctx.partial_results::<DocumentSymbolRequest>().is_none());
    }

    #[test]
    fn partial_result_reports_use_bounded_outbound_admission() {
        let (ctx, mut out_rx) = partial_result_context(1);
        let sink = ctx
            .partial_results::<DocumentSymbolRequest>()
            .expect("a supported request with a token has a sink");

        sink.report(DocumentSymbolPartialResponse::DocumentSymbolList(Vec::new()))
            .expect("the first chunk occupies the only outbound slot");
        assert!(
            out_rx.try_recv().is_ok(),
            "the chunk enters the outbound queue"
        );
        assert!(matches!(
            sink.report(DocumentSymbolPartialResponse::DocumentSymbolList(Vec::new())),
            Err(ClientError::OutboundOverloaded)
        ));
    }

    #[test]
    fn partial_result_scope_rejects_reports_after_request_completion() {
        let (ctx, mut out_rx) = partial_result_context(2);
        let scope = ctx.partial_result_scope().unwrap();
        let sink = ctx.partial_results::<DocumentSymbolRequest>().unwrap();

        scope.finish();

        assert!(matches!(
            sink.report(DocumentSymbolPartialResponse::DocumentSymbolList(Vec::new())),
            Err(ClientError::InvalidHelperParams(message))
                if message == "partial-result request has completed"
        ));
        assert!(out_rx.try_recv().is_err(), "a late chunk is not enqueued");
    }
}
