//! The connection's workspace-folder state (ADR 0017, ADR 0018).
//!
//! [`Workspace`] is the cloneable handle to the workspace-folder and root
//! information the client announces in `InitializeParams`. The protocol engine
//! establishes it during the initialize transaction — before `on_initialize`
//! runs — and later hooks observe the established state. Document contents live
//! in [`Documents`](crate::Documents), never here.
//!
//! This slice establishes the folders and root from `InitializeParams`;
//! `workspace/didChangeWorkspaceFolders` mutation and the configuration handle
//! arrive in later slices.

use std::sync::Arc;

use lsp_types::{InitializeParams, Uri, WorkspaceFolder};

/// Cloneable handle to the connection's workspace-folder state (ADR 0017).
///
/// Cheap to clone: every copy shares one `Arc`. The engine owns construction
/// from `InitializeParams`; user code only reads it through
/// [`Context::workspace`](crate::Context::workspace).
#[derive(Debug, Clone, Default)]
pub struct Workspace {
    inner: Arc<WorkspaceState>,
}

#[derive(Debug, Default)]
struct WorkspaceState {
    root_uri: Option<Uri>,
    folders: Vec<WorkspaceFolder>,
}

impl Workspace {
    /// Establish the workspace from the client's `InitializeParams` (ADR 0018).
    ///
    /// Captures the (possibly deprecated) `rootUri` and the announced
    /// workspace folders; an absent `workspaceFolders` list yields no folders.
    pub(crate) fn from_params(params: &InitializeParams) -> Self {
        #[allow(deprecated)] // `root_uri` is deprecated in LSP but still the input we echo.
        let root_uri = params.root_uri.clone();
        Self {
            inner: Arc::new(WorkspaceState {
                root_uri,
                folders: params.workspace_folders.clone().unwrap_or_default(),
            }),
        }
    }

    /// The client's announced root URI, if any.
    pub fn root_uri(&self) -> Option<&Uri> {
        self.inner.root_uri.as_ref()
    }

    /// The workspace folders announced at initialization.
    pub fn folders(&self) -> &[WorkspaceFolder] {
        &self.inner.folders
    }
}
