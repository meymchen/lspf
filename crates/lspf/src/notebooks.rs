use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use gen_lsp_types::{LspObject, NotebookCell, NotebookDocument, Uri};

use crate::uri_key::UriKey;

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

    #[allow(dead_code)] // Used by notebook lifecycle handling introduced in issue #251.
    pub(crate) fn open(&self, document: NotebookDocument) {
        let notebook = Notebook::from_document(document);
        let notebook_key = UriKey::new(notebook.uri());
        let mut inner = self.inner.write().unwrap();
        if let Some(previous) = inner.by_uri.remove(&notebook_key) {
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
