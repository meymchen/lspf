//! The connection's notebook layer (ADR 0034).
//!
//! [`Notebooks`] holds notebook metadata and ordered cell membership; cell text
//! stays in the connection's [`Documents`](crate::documents::Documents), which
//! is why nothing here touches a rope. The protocol engine owns every mutation
//! and drives the two stores together for one notebook notification.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use gen_lsp_types::{
    DidChangeNotebookDocumentParams, LspObject, NotebookCell, NotebookDocument, Uri,
};

use crate::error::LspError;
use crate::uri_key::UriKey;

const NOTEBOOK_NOT_FOUND: &str = "notebook not found";
const CELL_STRUCTURE_OUT_OF_RANGE: &str = "notebook cell structure change is out of range";
const UNKNOWN_CELL_DATA: &str = "notebook cell data change names an unknown cell";

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

#[derive(Debug, Default)]
struct NotebooksInner {
    by_uri: HashMap<UriKey, Notebook>,
    notebook_by_cell: HashMap<UriKey, UriKey>,
}

/// Concurrency-safe store of synchronized notebooks owned by one connection.
#[derive(Debug, Clone, Default)]
pub(crate) struct Notebooks {
    inner: Arc<RwLock<NotebooksInner>>,
}

impl Notebooks {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Register a notebook, returning the notebook the same URI already held.
    ///
    /// A replaced notebook's cell text Documents are the caller's to close:
    /// the notebook layer never touches the document store.
    pub(crate) fn open(&self, document: NotebookDocument) -> Option<Notebook> {
        self.commit(Notebook::from_document(document))
    }

    /// Install `notebook` as the state for its URI, rebuilding the cell index,
    /// and return the notebook it replaced.
    pub(crate) fn commit(&self, notebook: Notebook) -> Option<Notebook> {
        let notebook_key = UriKey::new(notebook.uri());
        let mut inner = self.inner.write().unwrap();
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

    /// Remove a notebook, returning it so the caller can close the cell text
    /// Documents it listed. `None` means the notebook was never synchronized.
    pub(crate) fn close(&self, uri: &Uri) -> Option<Notebook> {
        let mut inner = self.inner.write().unwrap();
        let removed = inner.by_uri.remove(&UriKey::new(uri))?;
        for cell in removed.cells() {
            inner.notebook_by_cell.remove(&UriKey::new(&cell.document));
        }
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
    /// engine mutate cell text Documents in between: a change whose splice is
    /// out of range is refused before either store moves, so the skipped hook
    /// has no partial state to observe.
    ///
    /// The three parts of a cell change are independent and apply in the order
    /// the LSP specifies: metadata, the structural splice, then per-cell data.
    /// Cell text content is not part of the plan; it belongs to the document
    /// store.
    pub(crate) fn plan_change(
        &self,
        params: &DidChangeNotebookDocumentParams,
    ) -> Result<Notebook, LspError> {
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
    }

    fn get(&self, uri: &Uri) -> Option<Notebook> {
        self.inner
            .read()
            .unwrap()
            .by_uri
            .get(&UriKey::new(uri))
            .cloned()
    }

    pub(crate) fn notebook_for_cell(&self, cell_uri: &Uri) -> Option<Notebook> {
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
        let notebooks = Notebooks::new();
        let notebook_uri = uri("file:///analysis.ipynb");
        notebooks.open(NotebookDocument::new(
            notebook_uri.clone(),
            "jupyter-notebook".into(),
            1,
            None,
            cells,
        ));
        (notebooks, notebook_uri)
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

        let planned = notebooks
            .plan_change(&change(
                &notebook_uri,
                2,
                None,
                Some(structural(1, 0, vec![cell("file:///c3")])),
            ))
            .expect("an in-range insertion is accepted");

        assert_eq!(
            cell_uris(&planned),
            ["file:///c1", "file:///c3", "file:///c2"],
            "a zero delete count inserts without removing"
        );
        assert_eq!(planned.version(), 2);
    }

    #[test]
    fn a_splice_replaces_the_deleted_range() {
        let (notebooks, notebook_uri) = opened(vec![
            cell("file:///c1"),
            cell("file:///c2"),
            cell("file:///c3"),
        ]);

        let planned = notebooks
            .plan_change(&change(
                &notebook_uri,
                2,
                None,
                Some(structural(0, 2, vec![cell("file:///c4")])),
            ))
            .expect("an in-range replacement is accepted");

        assert_eq!(cell_uris(&planned), ["file:///c4", "file:///c3"]);
    }

    #[test]
    fn a_plan_is_not_committed_until_the_caller_commits_it() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1")]);

        let planned = notebooks
            .plan_change(&change(
                &notebook_uri,
                2,
                None,
                Some(structural(1, 0, vec![cell("file:///c2")])),
            ))
            .expect("the insertion is in range");

        let before = notebooks.get(&notebook_uri).expect("the notebook is open");
        assert_eq!(
            cell_uris(&before),
            ["file:///c1"],
            "planning mutates nothing"
        );
        assert_eq!(before.version(), 1);

        notebooks.commit(planned);

        let after = notebooks.get(&notebook_uri).expect("the notebook is open");
        assert_eq!(cell_uris(&after), ["file:///c1", "file:///c2"]);
        assert_eq!(
            notebooks
                .notebook_for_cell(&uri("file:///c2"))
                .expect("the committed cell resolves to its notebook")
                .uri(),
            &notebook_uri,
            "commit rebuilds the cell index"
        );
    }

    #[test]
    fn an_out_of_range_splice_is_a_protocol_error_rather_than_a_panic() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1"), cell("file:///c2")]);

        for (start, delete_count) in [(3, 0), (0, 3), (1, 2), (u32::MAX, u32::MAX)] {
            let error = notebooks
                .plan_change(&change(
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
            .plan_change(&change(&uri("file:///other.ipynb"), 2, None, None))
            .expect_err("an unknown notebook is refused");

        assert!(error.to_string().contains(NOTEBOOK_NOT_FOUND));
    }

    #[test]
    fn metadata_and_cell_data_changes_apply_independently() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1"), cell("file:///c2")]);
        let updated = NotebookCell::new(
            NotebookCellKind::Markup,
            uri("file:///c2"),
            Some(object(json!({ "collapsed": true }))),
            None,
        );

        let planned = notebooks
            .plan_change(&change(
                &notebook_uri,
                2,
                Some(object(json!({ "kernel": "python3" }))),
                Some(NotebookDocumentCellChanges {
                    structure: None,
                    data: Some(vec![updated.clone()]),
                    text_content: None,
                }),
            ))
            .expect("a data-only change is accepted");

        assert_eq!(
            planned.metadata(),
            Some(&object(json!({ "kernel": "python3" })))
        );
        assert_eq!(planned.cells(), [cell("file:///c1"), updated]);
    }

    #[test]
    fn cell_data_for_an_unknown_cell_is_a_protocol_error() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1")]);

        let error = notebooks
            .plan_change(&change(
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
    fn closing_a_notebook_returns_it_and_forgets_its_cells() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1")]);

        let removed = notebooks
            .close(&notebook_uri)
            .expect("the open notebook is returned");

        assert_eq!(cell_uris(&removed), ["file:///c1"]);
        assert!(notebooks.get(&notebook_uri).is_none());
        assert!(
            notebooks.notebook_for_cell(&uri("file:///c1")).is_none(),
            "close drops the cell index too"
        );
        assert!(
            notebooks.close(&notebook_uri).is_none(),
            "closing an unsynchronized notebook removes nothing"
        );
    }

    #[test]
    fn reopening_a_notebook_returns_the_notebook_it_replaced() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1")]);

        let replaced = notebooks
            .open(NotebookDocument::new(
                notebook_uri.clone(),
                "jupyter-notebook".into(),
                2,
                None,
                vec![cell("file:///c2")],
            ))
            .expect("the previous notebook is handed back");

        assert_eq!(cell_uris(&replaced), ["file:///c1"]);
        assert!(
            notebooks.notebook_for_cell(&uri("file:///c1")).is_none(),
            "the replaced notebook's cells leave the index"
        );
        assert!(notebooks.notebook_for_cell(&uri("file:///c2")).is_some());
    }

    #[test]
    fn clear_releases_every_notebook_and_cell() {
        let (notebooks, notebook_uri) = opened(vec![cell("file:///c1")]);

        notebooks.clear();

        assert!(notebooks.get(&notebook_uri).is_none());
        assert!(notebooks.notebook_for_cell(&uri("file:///c1")).is_none());
    }
}
