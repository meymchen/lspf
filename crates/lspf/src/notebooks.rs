//! Connection-owned Notebook synchronization (ADR 0034).
//!
//! [`Notebooks`] holds notebook metadata and ordered cell membership; cell text
//! stays in the connection's [`Documents`](crate::documents::Documents), which
//! is why nothing here touches a rope. Each mutation coordinates both stores
//! before returning to the protocol engine for post-mutation hook dispatch.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use gen_lsp_types::{
    DidChangeNotebookDocumentParams, DidCloseNotebookDocumentParams, DidOpenNotebookDocumentParams,
    LspObject, NotebookCell, NotebookDocument, NotebookDocumentCellChanges, TextDocumentItem, Uri,
};

use tracing::debug;

use crate::documents::{Document, DocumentMutationError, Documents};
use crate::error::LspError;
use crate::telemetry::{ConnectionTrace, Resource, ResourceAction};
use crate::uri_key::UriKey;

const NOTEBOOK_COUNT_CAPACITY_EXHAUSTED: &str = "notebook count capacity exhausted";

const NOTEBOOK_NOT_FOUND: &str = "notebook not found";
const CELL_STRUCTURE_OUT_OF_RANGE: &str = "notebook cell structure change is out of range";
const UNKNOWN_CELL_DATA: &str = "notebook cell data change names an unknown cell";

#[derive(Debug, thiserror::Error)]
pub(crate) enum NotebookMutationError {
    #[error(transparent)]
    Protocol(LspError),
    #[error(transparent)]
    Capacity(LspError),
}

impl From<LspError> for NotebookMutationError {
    fn from(error: LspError) -> Self {
        Self::Protocol(error)
    }
}

impl From<DocumentMutationError> for NotebookMutationError {
    fn from(error: DocumentMutationError) -> Self {
        match error {
            DocumentMutationError::Protocol(error) => Self::Protocol(error),
            DocumentMutationError::Capacity(error) => Self::Capacity(error),
        }
    }
}

struct CellOpenRollback {
    uri: Uri,
    previous: Option<Document>,
}

/// A snapshot of one synchronized notebook.
///
/// The snapshot contains notebook metadata and ordered cell membership only.
/// Cell text remains in the connection's [`DocumentsView`](crate::DocumentsView).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notebook {
    uri: Uri,
    notebook_type: String,
    version: i32,
    metadata: Option<LspObject>,
    cells: Vec<NotebookCell>,
}

impl Notebook {
    fn from_document(document: NotebookDocument) -> Self {
        Self {
            uri: document.uri,
            notebook_type: document.notebook_type,
            version: document.version,
            metadata: document.metadata,
            cells: document.cells,
        }
    }

    /// The notebook URI supplied by the client.
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// The notebook type supplied by the client.
    pub fn notebook_type(&self) -> &str {
        &self.notebook_type
    }

    /// The editor-managed notebook version.
    pub fn version(&self) -> i32 {
        self.version
    }

    /// The client-supplied notebook metadata.
    pub fn metadata(&self) -> Option<&LspObject> {
        self.metadata.as_ref()
    }

    /// The notebook's cells in document order.
    pub fn cells(&self) -> &[NotebookCell] {
        &self.cells
    }
}

#[derive(Debug)]
struct NotebooksInner {
    by_uri: HashMap<UriKey, Notebook>,
    notebook_by_cell: HashMap<UriKey, UriKey>,
    max_notebooks: usize,
}

/// Coordinates Notebook mutations and their cell Documents for one connection.
///
/// The engine serializes mutations and runs hooks only after success. Existing
/// readers can observe intermediate cell mutations; separate view reads do not
/// promise a common revision. New cells consume budgets before old cells close.
#[derive(Debug, Clone)]
pub(crate) struct Notebooks {
    inner: Arc<RwLock<NotebooksInner>>,
    documents: Documents,
    trace: ConnectionTrace,
}

impl Default for Notebooks {
    fn default() -> Self {
        let policy = crate::ResourcePolicy::default();
        let trace = ConnectionTrace::new();
        Self::with_resource_policy(
            Documents::with_resource_policy(policy, trace),
            policy,
            trace,
        )
    }
}

impl Notebooks {
    #[cfg(test)]
    pub(crate) fn new(documents: Documents) -> Self {
        Self::with_resource_policy(
            documents,
            crate::ResourcePolicy::default(),
            ConnectionTrace::new(),
        )
    }

    pub(crate) fn with_resource_policy(
        documents: Documents,
        policy: crate::ResourcePolicy,
        trace: ConnectionTrace,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(NotebooksInner {
                by_uri: HashMap::new(),
                notebook_by_cell: HashMap::new(),
                max_notebooks: policy.max_notebooks,
            })),
            documents,
            trace,
        }
    }

    /// Open a Notebook and its cell Documents, restoring every cell on refusal.
    pub(crate) fn open(
        &self,
        params: DidOpenNotebookDocumentParams,
    ) -> Result<(), NotebookMutationError> {
        let opened = self.open_cell_text_documents(params.cell_text_documents)?;
        let superseded = match self.insert(params.notebook_document) {
            Ok(superseded) => superseded,
            Err(error) => {
                self.rollback_cell_opens(opened);
                return Err(NotebookMutationError::Capacity(error));
            }
        };
        if let Some(superseded) = superseded {
            debug!(uri = ?superseded.uri(), "re-opening a notebook that was already open");
            self.close_departed_cells(&superseded);
        }
        Ok(())
    }

    /// Close listed and explicitly named cell Documents, even for an unknown Notebook.
    pub(crate) fn close(&self, params: DidCloseNotebookDocumentParams) {
        match self.remove(&params.notebook_document.uri) {
            Some(removed) => {
                for cell in removed.cells() {
                    self.documents.close(&cell.document);
                }
            }
            None => debug!(
                uri = ?params.notebook_document.uri,
                "closing a notebook that was not open"
            ),
        }
        for cell in params.cell_text_documents {
            self.documents.close(&cell.uri);
        }
    }

    /// Apply one complete Notebook change, restoring cell Documents on refusal.
    ///
    /// The order is what makes the whole notification all-or-nothing. Planning
    /// the notebook validates the splice without committing it; every cell text
    /// Document mutation that can be refused runs next, undoing its own work on
    /// refusal; the notebook is committed only once nothing can fail; and the
    /// cells that left the notebook — the one irreversible step — close last.
    pub(crate) fn change(
        &self,
        params: DidChangeNotebookDocumentParams,
    ) -> std::result::Result<(), NotebookMutationError> {
        let planned = self.plan_change(&params)?;
        let closed_cells = match params.change.cells {
            Some(cells) => self.apply_cell_text_documents(cells)?,
            None => Vec::new(),
        };
        // A cell the splice removed loses its Document here even when the peer
        // left it out of `didClose`. Nothing afterwards can name that cell, so
        // trusting `didClose` alone is how a cell text Document leaks.
        if let Some(superseded) = self.commit(planned) {
            self.close_departed_cells(&superseded);
        }
        for uri in closed_cells {
            self.documents.close(&uri);
        }
        Ok(())
    }

    /// Mutate the cell text Documents one cell change names — the structural
    /// change's opens, then every cell's text content — and return the URIs the
    /// peer asked to close, which the caller closes after the notebook commits.
    ///
    /// Any refusal restores what this notification already did: the cells it
    /// opened close again and the cells it edited return to their prior text
    /// and version, so the skipped hook has no partial state to observe.
    fn apply_cell_text_documents(
        &self,
        cells: NotebookDocumentCellChanges,
    ) -> std::result::Result<Vec<Uri>, NotebookMutationError> {
        let (opened_cells, closed_cells) = match cells.structure {
            Some(structure) => (
                structure.did_open.unwrap_or_default(),
                structure.did_close.unwrap_or_default(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        let opened = self.open_cell_text_documents(opened_cells)?;

        // Cell text reuses the document store's incremental change path, so a
        // cell edit behaves exactly like `textDocument/didChange` on the same
        // URI — including its all-or-nothing batch and its byte budget.
        let mut edited = Vec::new();
        for cell in cells.text_content.unwrap_or_default() {
            let previous = self
                .documents
                .get(&cell.document.text_document_identifier.uri);
            match self.documents.apply_changes(
                &cell.document.text_document_identifier.uri,
                cell.document.version,
                cell.changes,
            ) {
                Ok(()) => edited.extend(previous),
                Err(error) => {
                    // A URI can occur more than once, including through an
                    // equivalent spelling. Undo the latest snapshot first so
                    // the original text, version, and accounting win last.
                    for document in edited.into_iter().rev() {
                        self.documents.restore(document);
                    }
                    self.rollback_cell_opens(opened);
                    return Err(error.into());
                }
            }
        }

        Ok(closed_cells.into_iter().map(|cell| cell.uri).collect())
    }

    /// Open every cell text Document, retaining the prior snapshot needed to
    /// roll back this notification.
    ///
    /// A refusal undoes the opens made so far, so a rejected notebook
    /// notification leaves no orphan or overwritten cell text Document behind.
    fn open_cell_text_documents(
        &self,
        cells: Vec<TextDocumentItem>,
    ) -> std::result::Result<Vec<CellOpenRollback>, DocumentMutationError> {
        let mut opened = Vec::new();
        for cell in cells {
            let uri = cell.uri.clone();
            let previous = self.documents.get(&uri);
            if let Err(error) = self.documents.open(cell) {
                self.rollback_cell_opens(opened);
                return Err(error);
            }
            opened.push(CellOpenRollback { uri, previous });
        }
        Ok(opened)
    }

    fn rollback_cell_opens(&self, opened: Vec<CellOpenRollback>) {
        for opened in opened.into_iter().rev() {
            match opened.previous {
                Some(document) => self.documents.restore(document),
                None => {
                    self.documents.close(&opened.uri);
                }
            }
        }
    }

    /// Close the cell text Documents a superseded notebook listed that no
    /// synchronized notebook lists any more.
    fn close_departed_cells(&self, superseded: &Notebook) {
        for cell in superseded.cells() {
            if self.notebook_for_cell(&cell.document).is_none() {
                self.documents.close(&cell.document);
            }
        }
    }

    fn insert(&self, document: NotebookDocument) -> Result<Option<Notebook>, LspError> {
        let notebook = Notebook::from_document(document);
        let notebook_key = UriKey::new(notebook.uri());
        let mut inner = self.inner.write().unwrap();
        if !inner.by_uri.contains_key(&notebook_key) && inner.by_uri.len() >= inner.max_notebooks {
            self.trace.resource_budget(
                Resource::Notebooks,
                ResourceAction::Reject,
                inner.by_uri.len(),
                inner.max_notebooks,
                None,
            );
            return Err(LspError::invalid_request(NOTEBOOK_COUNT_CAPACITY_EXHAUSTED));
        }
        let previous = commit(&mut inner, notebook_key, notebook);
        self.trace.resource_budget(
            Resource::Notebooks,
            ResourceAction::Admit,
            inner.by_uri.len(),
            inner.max_notebooks,
            None,
        );
        Ok(previous)
    }

    /// Install `notebook` as the state for its URI, rebuilding the cell index,
    /// and return the notebook it replaced.
    fn commit(&self, notebook: Notebook) -> Option<Notebook> {
        let notebook_key = UriKey::new(notebook.uri());
        let mut inner = self.inner.write().unwrap();
        let previous = commit(&mut inner, notebook_key, notebook);
        self.trace.resource_budget(
            Resource::Notebooks,
            ResourceAction::Update,
            inner.by_uri.len(),
            inner.max_notebooks,
            None,
        );
        previous
    }

    /// Remove Notebook metadata and its cell index.
    /// `None` means the notebook was never synchronized.
    fn remove(&self, uri: &Uri) -> Option<Notebook> {
        let mut inner = self.inner.write().unwrap();
        let removed = inner.by_uri.remove(&UriKey::new(uri))?;
        for cell in removed.cells() {
            inner.notebook_by_cell.remove(&UriKey::new(&cell.document));
        }
        self.trace.resource_budget(
            Resource::Notebooks,
            ResourceAction::Release,
            inner.by_uri.len(),
            inner.max_notebooks,
            None,
        );
        Some(removed)
    }

    /// Whether `uri` names a synchronized notebook.
    pub(crate) fn contains(&self, uri: &Uri) -> bool {
        self.inner
            .read()
            .unwrap()
            .by_uri
            .contains_key(&UriKey::new(uri))
    }

    /// Compute the notebook state one `notebookDocument/didChange` produces,
    /// without committing it.
    ///
    /// Planning separately from [`commit`](Self::commit) is what lets the
    /// cell text mutation run in between: a change whose splice is
    /// out of range is refused before either store moves, so the skipped hook
    /// has no partial state to observe.
    ///
    /// The three parts of a cell change are independent and apply in the order
    /// the LSP specifies: metadata, the structural splice, then per-cell data.
    /// Cell text content is not part of the plan; it belongs to the document
    /// store.
    fn plan_change(&self, params: &DidChangeNotebookDocumentParams) -> Result<Notebook, LspError> {
        let mut notebook = self
            .get(&params.notebook_document.uri)
            .ok_or_else(|| LspError::invalid_request(NOTEBOOK_NOT_FOUND))?;
        notebook.version = params.notebook_document.version;
        if params.change.metadata.is_some() {
            notebook.metadata = params.change.metadata.clone();
        }
        let Some(cells) = params.change.cells.as_ref() else {
            return Ok(notebook);
        };
        if let Some(structure) = &cells.structure {
            // The peer supplies the start, the delete count, and the
            // replacement cells; any of the three can name cells this notebook
            // does not have. `start <= start + delete_count`, so bounding the
            // end also bounds the start and the splice range cannot invert.
            let start = structure.array.start as usize;
            let end = start
                .checked_add(structure.array.delete_count as usize)
                .filter(|end| *end <= notebook.cells.len())
                .ok_or_else(|| LspError::invalid_request(CELL_STRUCTURE_OUT_OF_RANGE))?;
            let replacement = structure.array.cells.clone().unwrap_or_default();
            notebook.cells.splice(start..end, replacement);
        }
        if let Some(data) = &cells.data {
            for updated in data {
                let key = UriKey::new(&updated.document);
                let cell = notebook
                    .cells
                    .iter_mut()
                    .find(|cell| UriKey::new(&cell.document) == key)
                    .ok_or_else(|| LspError::invalid_request(UNKNOWN_CELL_DATA))?;
                *cell = updated.clone();
            }
        }
        Ok(notebook)
    }

    /// Release every notebook the connection tracked.
    pub(crate) fn clear(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.by_uri = HashMap::new();
        inner.notebook_by_cell = HashMap::new();
        self.trace.resource_budget(
            Resource::Notebooks,
            ResourceAction::Release,
            0,
            inner.max_notebooks,
            None,
        );
    }

    fn get(&self, uri: &Uri) -> Option<Notebook> {
        self.inner
            .read()
            .unwrap()
            .by_uri
            .get(&UriKey::new(uri))
            .cloned()
    }

    fn notebook_for_cell(&self, cell_uri: &Uri) -> Option<Notebook> {
        let inner = self.inner.read().unwrap();
        let notebook_uri = inner.notebook_by_cell.get(&UriKey::new(cell_uri))?;
        inner.by_uri.get(notebook_uri).cloned()
    }

    pub(crate) fn view(&self) -> NotebooksView {
        NotebooksView {
            notebooks: self.clone(),
        }
    }
}

fn commit(
    inner: &mut NotebooksInner,
    notebook_key: UriKey,
    notebook: Notebook,
) -> Option<Notebook> {
    let previous = inner.by_uri.remove(&notebook_key);
    if let Some(previous) = &previous {
        for cell in previous.cells() {
            inner.notebook_by_cell.remove(&UriKey::new(&cell.document));
        }
    }
    for cell in notebook.cells() {
        inner
            .notebook_by_cell
            .insert(UriKey::new(&cell.document), notebook_key.clone());
    }
    inner.by_uri.insert(notebook_key, notebook);
    previous
}

/// Read-only access to the synchronized notebooks owned by one connection.
///
/// Cheap to clone: every copy observes the same store, while returned
/// [`Notebook`] values are snapshots that user code can inspect but cannot use
/// to mutate connection state.
#[derive(Debug, Clone)]
pub struct NotebooksView {
    notebooks: Notebooks,
}

impl NotebooksView {
    /// Read a notebook snapshot by URI, or `None` if it is not synchronized.
    pub fn get(&self, uri: &Uri) -> Option<Notebook> {
        self.notebooks.get(uri)
    }

    /// Read the notebook containing `cell_uri`, or `None` if it is not a cell
    /// in any synchronized notebook.
    pub fn notebook_for_cell(&self, cell_uri: &Uri) -> Option<Notebook> {
        self.notebooks.notebook_for_cell(cell_uri)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::str::FromStr;

    use gen_lsp_types::{
        NotebookCellArrayChange, NotebookCellKind, NotebookDocumentCellChangeStructure,
        NotebookDocumentCellChanges, NotebookDocumentChangeEvent,
        VersionedNotebookDocumentIdentifier,
    };
    use serde_json::json;

    use super::*;

    fn uri(spelling: &str) -> Uri {
        Uri::from_str(spelling).expect("the test URI parses")
    }

    fn cell(spelling: &str) -> NotebookCell {
        NotebookCell::new(NotebookCellKind::Code, uri(spelling), None, None)
    }

    fn object(value: serde_json::Value) -> LspObject {
        serde_json::from_value(value).expect("the test metadata is an object")
    }

    /// A notebook with `cells` already synchronized, plus the store holding it.
    fn opened(cells: Vec<NotebookCell>) -> (Notebooks, Uri) {
        let notebooks = Notebooks::default();
        let notebook_uri = uri("file:///analysis.ipynb");
        notebooks
            .open(open_params(&notebook_uri, 1, cells))
            .unwrap();
        (notebooks, notebook_uri)
    }

    fn open_params(
        uri: &Uri,
        version: i32,
        cells: Vec<NotebookCell>,
    ) -> DidOpenNotebookDocumentParams {
        let cell_text_documents = cells
            .iter()
            .map(|cell| TextDocumentItem {
                uri: cell.document.clone(),
                language_id: "python".into(),
                version: 1,
                text: "one".into(),
            })
            .collect();
        DidOpenNotebookDocumentParams {
            notebook_document: NotebookDocument::new(
                uri.clone(),
                "jupyter-notebook".into(),
                version,
                None,
                cells,
            ),
            cell_text_documents,
        }
    }

    fn close_params(uri: &Uri) -> DidCloseNotebookDocumentParams {
        serde_json::from_value(json!({
            "notebookDocument": {"uri": uri}, "cellTextDocuments": []
        }))
        .unwrap()
    }

    fn with_policy(policy: crate::ResourcePolicy) -> (Notebooks, crate::DocumentsView) {
        let trace = ConnectionTrace::new();
        let documents = Documents::with_resource_policy(policy, trace);
        let view = documents.view();
        (
            Notebooks::with_resource_policy(documents, policy, trace),
            view,
        )
    }

    fn changed_cells(value: serde_json::Value) -> NotebookDocumentCellChanges {
        serde_json::from_value(value).unwrap()
    }

    fn structural(
        start: u32,
        delete_count: u32,
        cells: Vec<NotebookCell>,
    ) -> NotebookDocumentCellChanges {
        NotebookDocumentCellChanges {
            structure: Some(NotebookDocumentCellChangeStructure {
                array: NotebookCellArrayChange::new(start, delete_count, Some(cells)),
                did_open: None,
                did_close: None,
            }),
            data: None,
            text_content: None,
        }
    }

    fn cell_uris(notebook: &Notebook) -> Vec<String> {
        notebook
            .cells()
            .iter()
            .map(|cell| cell.document.as_str().to_string())
            .collect()
    }

    /// One `notebookDocument/didChange` against the test notebook.
    fn change(
        uri: &Uri,
        version: i32,
        metadata: Option<LspObject>,
        cells: Option<NotebookDocumentCellChanges>,
    ) -> DidChangeNotebookDocumentParams {
        DidChangeNotebookDocumentParams::new(
            VersionedNotebookDocumentIdentifier::new(version, uri.clone()),
            NotebookDocumentChangeEvent::new(metadata, cells),
        )
    }

    #[test]
    fn a_splice_inserts_replacement_cells_at_the_start_index() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1"), cell("file:///c2")]);

        notebooks
            .change(change(
                &notebook_uri,
                2,
                None,
                Some(structural(1, 0, vec![cell("file:///c3")])),
            ))
            .expect("an in-range insertion is accepted");
        let updated = notebooks.view().get(&notebook_uri).unwrap();

        assert_eq!(
            cell_uris(&updated),
            ["file:///c1", "file:///c3", "file:///c2"],
            "a zero delete count inserts without removing"
        );
        assert_eq!(updated.version(), 2);
    }

    #[test]
    fn a_splice_replaces_the_deleted_range() {
        let (notebooks, notebook_uri) = opened(vec![
            cell("file:///c1"),
            cell("file:///c2"),
            cell("file:///c3"),
        ]);

        notebooks
            .change(change(
                &notebook_uri,
                2,
                None,
                Some(structural(0, 2, vec![cell("file:///c4")])),
            ))
            .expect("an in-range replacement is accepted");
        let updated = notebooks.view().get(&notebook_uri).unwrap();

        assert_eq!(cell_uris(&updated), ["file:///c4", "file:///c3"]);
    }

    #[test]
    fn an_out_of_range_splice_is_a_protocol_error_rather_than_a_panic() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1"), cell("file:///c2")]);

        for (start, delete_count) in [(3, 0), (0, 3), (1, 2), (u32::MAX, u32::MAX)] {
            let error = notebooks
                .change(change(
                    &notebook_uri,
                    2,
                    None,
                    Some(structural(start, delete_count, Vec::new())),
                ))
                .expect_err("an out-of-range splice is refused");
            assert!(
                error.to_string().contains(CELL_STRUCTURE_OUT_OF_RANGE),
                "start {start} delete_count {delete_count} reports the range error: {error}"
            );
        }
    }

    #[test]
    fn changing_an_unsynchronized_notebook_is_a_protocol_error() {
        let (notebooks, _) = opened(vec![cell("file:///c1")]);

        let error = notebooks
            .change(change(&uri("file:///other.ipynb"), 2, None, None))
            .expect_err("an unknown notebook is refused");

        assert!(error.to_string().contains(NOTEBOOK_NOT_FOUND));
    }

    #[test]
    fn metadata_and_cell_data_changes_apply_independently() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1"), cell("file:///c2")]);
        let updated_cell = NotebookCell::new(
            NotebookCellKind::Markup,
            uri("file:///c2"),
            Some(object(json!({ "collapsed": true }))),
            None,
        );

        notebooks
            .change(change(
                &notebook_uri,
                2,
                Some(object(json!({ "kernel": "python3" }))),
                Some(NotebookDocumentCellChanges {
                    structure: None,
                    data: Some(vec![updated_cell.clone()]),
                    text_content: None,
                }),
            ))
            .expect("a data-only change is accepted");
        let updated = notebooks.view().get(&notebook_uri).unwrap();

        assert_eq!(
            updated.metadata(),
            Some(&object(json!({ "kernel": "python3" })))
        );
        assert_eq!(updated.cells(), [cell("file:///c1"), updated_cell]);
    }

    #[test]
    fn cell_data_for_an_unknown_cell_is_a_protocol_error() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1")]);

        let error = notebooks
            .change(change(
                &notebook_uri,
                2,
                None,
                Some(NotebookDocumentCellChanges {
                    structure: None,
                    data: Some(vec![cell("file:///absent")]),
                    text_content: None,
                }),
            ))
            .expect_err("data for a cell outside the notebook is refused");

        assert!(error.to_string().contains(UNKNOWN_CELL_DATA));
    }

    #[test]
    fn closing_a_notebook_forgets_its_cells() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1")]);
        notebooks.close(close_params(&notebook_uri));
        assert!(notebooks.view().get(&notebook_uri).is_none());
        assert!(
            notebooks
                .view()
                .notebook_for_cell(&uri("file:///c1"))
                .is_none()
        );
        notebooks.close(close_params(&notebook_uri));
        assert!(notebooks.view().get(&notebook_uri).is_none());
    }

    #[test]
    fn reopening_a_notebook_replaces_its_cell_index() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1")]);
        notebooks
            .open(open_params(&notebook_uri, 2, vec![cell("file:///c2")]))
            .unwrap();
        let updated = notebooks.view().get(&notebook_uri).unwrap();
        assert_eq!(updated.version(), 2);
        assert_eq!(cell_uris(&updated), ["file:///c2"]);
        assert!(
            notebooks
                .view()
                .notebook_for_cell(&uri("file:///c1"))
                .is_none()
        );
        assert!(
            notebooks
                .view()
                .notebook_for_cell(&uri("file:///c2"))
                .is_some()
        );
    }

    #[test]
    fn a_full_document_count_rejects_a_new_cell_before_closing_the_old_one() {
        let (notebooks, documents) = with_policy(crate::ResourcePolicy {
            max_documents: 1,
            ..crate::ResourcePolicy::default()
        });
        let notebook_uri = uri("file:///analysis.ipynb");
        notebooks
            .open(open_params(&notebook_uri, 1, vec![cell("file:///c1")]))
            .unwrap();
        let before = notebooks.view().get(&notebook_uri).unwrap();
        let error = notebooks.change(change(&notebook_uri, 2, None, Some(changed_cells(json!({
            "structure": {
                "array": {"start": 0, "deleteCount": 1, "cells": [{"kind": 2, "document": "file:///c2"}]},
                "didOpen": [{"uri": "file:///c2", "languageId": "python", "version": 1, "text": "two"}],
                "didClose": [{"uri": "file:///c1"}]
            }
        }))))).unwrap_err();
        assert!(matches!(error, NotebookMutationError::Capacity(_)));
        assert_eq!(notebooks.view().get(&notebook_uri), Some(before));
        assert_eq!(documents.get(&uri("file:///c1")).unwrap().text(), "one");
        assert!(documents.get(&uri("file:///c2")).is_none());
        notebooks.close(close_params(&notebook_uri));
        assert!(documents.get(&uri("file:///c1")).is_none());
        notebooks
            .open(open_params(&notebook_uri, 3, vec![cell("file:///c2")]))
            .unwrap();
        assert_eq!(documents.get(&uri("file:///c2")).unwrap().text(), "one");
    }

    #[test]
    fn rejected_edits_restore_text_version_identity_and_byte_capacity() {
        for repeated in ["file:///c1", "file:///c%31"] {
            let (notebooks, documents) = with_policy(crate::ResourcePolicy {
                max_document_bytes: 6,
                ..crate::ResourcePolicy::default()
            });
            let notebook_uri = uri("file:///analysis.ipynb");
            notebooks
                .open(open_params(
                    &notebook_uri,
                    1,
                    vec![cell("file:///c1"), cell("file:///c2")],
                ))
                .unwrap();
            let before = notebooks.view().get(&notebook_uri).unwrap();
            let error = notebooks.change(change(&notebook_uri, 2, None, Some(changed_cells(json!({
                "textContent": [
                    {"document": {"uri": "file:///c1", "version": 2}, "changes": [{"text": "x"}]},
                    {"document": {"uri": repeated, "version": 3}, "changes": [{"text": "yy"}]},
                    {"document": {"uri": "file:///c2", "version": 2}, "changes": [{"text": "too long"}]}
                ]
            }))))).unwrap_err();
            assert!(matches!(error, NotebookMutationError::Capacity(_)));
            assert_eq!(notebooks.view().get(&notebook_uri), Some(before));
            for spelling in ["file:///c1", "file:///c2"] {
                let document = documents.get(&uri(spelling)).unwrap();
                assert_eq!(document.text(), "one");
                assert_eq!(document.version(), Some(1));
                assert_eq!(document.uri(), &uri(spelling));
            }
            // A reopen exactly fills the byte budget, then one extra byte
            // must still be refused. Both overcount and undercount are visible.
            notebooks
                .open(open_params(
                    &notebook_uri,
                    3,
                    vec![cell("file:///c1"), cell("file:///c2")],
                ))
                .unwrap();
            let too_large = change(
                &notebook_uri,
                4,
                None,
                Some(changed_cells(json!({
                    "textContent": [{"document": {"uri": "file:///c2", "version": 4}, "changes": [{"text": "four"}]}]
                }))),
            );
            assert!(matches!(
                notebooks.change(too_large),
                Err(NotebookMutationError::Capacity(_))
            ));
        }
    }

    #[test]
    fn notebook_capacity_refusal_undoes_repeated_opens_and_new_documents() {
        let (notebooks, documents) = with_policy(crate::ResourcePolicy {
            max_notebooks: 1,
            max_documents: 2,
            max_document_bytes: 6,
            ..crate::ResourcePolicy::default()
        });
        let notebook_uri = uri("file:///analysis.ipynb");
        notebooks
            .open(open_params(&notebook_uri, 1, vec![cell("file:///c1")]))
            .unwrap();
        let other = uri("file:///other.ipynb");
        let mut rejected = open_params(
            &other,
            1,
            vec![cell("file:///c1"), cell("file:///c%31"), cell("file:///c2")],
        );
        rejected.cell_text_documents[0].text = "x".into();
        rejected.cell_text_documents[0].version = 2;
        rejected.cell_text_documents[1].text = "yy".into();
        rejected.cell_text_documents[1].version = 3;
        assert!(matches!(
            notebooks.open(rejected),
            Err(NotebookMutationError::Capacity(_))
        ));
        assert!(notebooks.view().get(&other).is_none());
        assert!(documents.get(&uri("file:///c2")).is_none());
        let restored = documents.get(&uri("file:///c%31")).unwrap();
        assert_eq!(restored.uri(), &uri("file:///c1"));
        assert_eq!(restored.text(), "one");
        assert_eq!(restored.version(), Some(1));
        assert_eq!(
            notebooks
                .view()
                .notebook_for_cell(&uri("file:///c1"))
                .unwrap()
                .uri(),
            &notebook_uri
        );
        notebooks
            .open(open_params(
                &notebook_uri,
                2,
                vec![cell("file:///c1"), cell("file:///c2")],
            ))
            .unwrap();
        assert_eq!(documents.get(&uri("file:///c2")).unwrap().text(), "one");
    }

    #[test]
    fn protocol_refusal_undoes_cell_opens_and_prior_text_edits() {
        let (notebooks, documents) = with_policy(crate::ResourcePolicy::default());
        let notebook_uri = uri("file:///analysis.ipynb");
        notebooks
            .open(open_params(&notebook_uri, 1, vec![cell("file:///c1")]))
            .unwrap();
        let before = notebooks.view().get(&notebook_uri).unwrap();
        let error = notebooks.change(change(&notebook_uri, 2, None, Some(changed_cells(json!({
            "structure": {
                "array": {"start": 1, "deleteCount": 0, "cells": [{"kind": 2, "document": "file:///c2"}]},
                "didOpen": [{"uri": "file:///c2", "languageId": "python", "version": 1, "text": "two"}]
            },
            "textContent": [
                {"document": {"uri": "file:///c1", "version": 2}, "changes": [{"text": "changed"}]},
                {"document": {"uri": "file:///absent", "version": 2}, "changes": [{"text": "invalid"}]}
            ]
        }))))).unwrap_err();
        assert!(matches!(error, NotebookMutationError::Protocol(_)));
        assert_eq!(notebooks.view().get(&notebook_uri), Some(before));
        assert!(documents.get(&uri("file:///c2")).is_none());
        let restored = documents.get(&uri("file:///c1")).unwrap();
        assert_eq!(restored.text(), "one");
        assert_eq!(restored.version(), Some(1));
    }

    #[test]
    fn reopens_and_closes_release_cell_documents_with_membership() {
        let (notebooks, documents) = with_policy(crate::ResourcePolicy::default());
        let notebook_uri = uri("file:///analysis.ipynb");
        notebooks
            .open(open_params(
                &notebook_uri,
                1,
                vec![cell("file:///c1"), cell("file:///c2")],
            ))
            .unwrap();
        notebooks
            .open(open_params(
                &notebook_uri,
                2,
                vec![cell("file:///c2"), cell("file:///c3")],
            ))
            .unwrap();
        assert!(documents.get(&uri("file:///c1")).is_none());
        assert!(documents.get(&uri("file:///c2")).is_some());
        assert!(documents.get(&uri("file:///c3")).is_some());
        let close = serde_json::from_value(json!({
            "notebookDocument": {"uri": "file:///unknown.ipynb"},
            "cellTextDocuments": [{"uri": "file:///c3"}]
        }))
        .unwrap();
        notebooks.close(close);
        assert!(documents.get(&uri("file:///c3")).is_none());
        notebooks.close(close_params(&notebook_uri));
        assert!(documents.get(&uri("file:///c2")).is_none());
        assert!(notebooks.view().get(&notebook_uri).is_none());
    }

    #[test]
    fn clear_releases_every_notebook_and_cell() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1")]);

        notebooks.clear();

        assert!(notebooks.view().get(&notebook_uri).is_none());
        assert!(
            notebooks
                .view()
                .notebook_for_cell(&uri("file:///c1"))
                .is_none()
        );
    }
}
