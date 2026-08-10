//! The connection's workspace state (ADR 0017, ADR 0018).
//!
//! [`Workspace`] is the cloneable handle to everything the client announces in
//! `InitializeParams`: client info, capabilities, initialization options, the
//! root URI, and the workspace folders. The protocol engine establishes it
//! during the initialize transaction — before `on_initialize` runs — and later
//! hooks observe the established state. It also owns the connection's
//! [`Documents`] handle, handing out the read-only [`DocumentsView`].
//!
//! Workspace-folder, configuration, and trace notifications update shared
//! live state before their user hooks run. Concurrent readers receive owned
//! snapshots, so cloned handles safely observe later protocol mutations.

use std::sync::{Arc, RwLock};

use lsp_types::{
    ClientCapabilities, ClientInfo, DidChangeWorkspaceFoldersParams, InitializeParams, TraceValue,
    Uri, WorkspaceFolder,
};
use serde_json::Value;

use crate::documents::{Documents, DocumentsView};
use crate::uri_key::{UriKey, percent_decode};

/// Cloneable handle to the connection's workspace state (ADR 0017).
///
/// Cheap to clone: every copy shares one `Arc`, so clones observe the same
/// connection state rather than a snapshot of it. The engine owns
/// construction from `InitializeParams`; user code only reads it through
/// [`Context::workspace`](crate::Context::workspace).
#[derive(Debug, Clone)]
pub struct Workspace {
    inner: Arc<WorkspaceState>,
}

#[derive(Debug)]
struct WorkspaceState {
    client_info: Option<ClientInfo>,
    capabilities: ClientCapabilities,
    initialization_options: Option<Value>,
    root_uri: Option<Uri>,
    folders: RwLock<Vec<WorkspaceFolder>>,
    configuration: RwLock<Option<Value>>,
    trace: RwLock<TraceValue>,
    documents: Documents,
}

impl Workspace {
    /// Establish the workspace from the client's `InitializeParams` (ADR 0018).
    ///
    /// Captures the client info, capabilities, initialization options, the
    /// (possibly deprecated) `rootUri`, and the announced workspace folders —
    /// all verbatim, folder order included; an absent `workspaceFolders` list
    /// yields no folders. `documents` is the connection's document store,
    /// which the workspace owns from here on.
    pub(crate) fn from_params(params: &InitializeParams, documents: Documents) -> Self {
        #[allow(deprecated)] // `root_uri` is deprecated in LSP but still the input we echo.
        let root_uri = params.root_uri.clone();
        Self {
            inner: Arc::new(WorkspaceState {
                client_info: params.client_info.clone(),
                capabilities: params.capabilities.clone(),
                initialization_options: params.initialization_options.clone(),
                root_uri,
                folders: RwLock::new(params.workspace_folders.clone().unwrap_or_default()),
                configuration: RwLock::new(None),
                trace: RwLock::new(TraceValue::Off),
                documents,
            }),
        }
    }

    /// The client info (`clientInfo.name` / `clientInfo.version`) announced at
    /// initialization, if the client sent any.
    pub fn client_info(&self) -> Option<&ClientInfo> {
        self.inner.client_info.as_ref()
    }

    /// The client capabilities announced at initialization, stored verbatim.
    pub fn capabilities(&self) -> &ClientCapabilities {
        &self.inner.capabilities
    }

    /// The client-provided initialization options, exactly as sent.
    pub fn initialization_options(&self) -> Option<&Value> {
        self.inner.initialization_options.as_ref()
    }

    /// The client's announced root URI, if any.
    pub fn root_uri(&self) -> Option<&Uri> {
        self.inner.root_uri.as_ref()
    }

    /// The current workspace folders as an owned, order-preserving snapshot.
    pub fn folders(&self) -> Vec<WorkspaceFolder> {
        self.inner.folders.read().unwrap().clone()
    }

    /// The latest raw `workspace/didChangeConfiguration` settings value.
    ///
    /// The framework neither interprets this value nor persists it outside
    /// the connection-owned workspace.
    pub fn configuration(&self) -> Option<Value> {
        self.inner.configuration.read().unwrap().clone()
    }

    /// The connection's current protocol trace level.
    pub fn trace(&self) -> TraceValue {
        *self.inner.trace.read().unwrap()
    }

    /// The effective workspace roots.
    ///
    /// Prefers the announced workspace folders; with none — an absent or
    /// empty list — falls back to one synthetic root derived from `rootUri`,
    /// named for its final path segment (percent-decoded, since the name is
    /// a display string) or `"workspace"` when there is none. With no
    /// folders and no `rootUri`, there are no roots.
    pub fn roots(&self) -> Vec<WorkspaceFolder> {
        let folders = self.folders();
        if !folders.is_empty() {
            return folders;
        }
        let Some(root_uri) = &self.inner.root_uri else {
            return Vec::new();
        };
        let name = root_uri
            .path()
            .as_str()
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .map(percent_decode)
            .unwrap_or_else(|| "workspace".to_string());
        vec![WorkspaceFolder {
            uri: root_uri.clone(),
            name,
        }]
    }

    /// The connection's documents, as a read-only [`DocumentsView`].
    ///
    /// The workspace owns the document store the engine mutates through its
    /// built-in document-sync handling; user code reads it through this view
    /// (or [`Context::documents`](crate::Context::documents), which is the
    /// same view).
    pub fn documents(&self) -> DocumentsView {
        self.inner.documents.view()
    }

    pub(crate) fn apply_folder_change(&self, params: DidChangeWorkspaceFoldersParams) {
        let mut folders = self.inner.folders.write().unwrap();
        for removed in params.event.removed {
            let key = UriKey::new(&removed.uri);
            if let Some(index) = folders
                .iter()
                .position(|folder| UriKey::new(&folder.uri) == key)
            {
                folders.remove(index);
            } else {
                tracing::debug!(uri = ?removed.uri, "removing an unknown workspace folder");
            }
        }
        for added in params.event.added {
            let key = UriKey::new(&added.uri);
            if let Some(existing) = folders
                .iter_mut()
                .find(|folder| UriKey::new(&folder.uri) == key)
            {
                *existing = added;
            } else {
                folders.push(added);
            }
        }
    }

    pub(crate) fn set_configuration(&self, settings: Value) {
        *self.inner.configuration.write().unwrap() = Some(settings);
    }

    pub(crate) fn set_trace(&self, trace: TraceValue) {
        *self.inner.trace.write().unwrap() = trace;
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Barrier;

    use lsp_types::{
        ClientCapabilities, ClientInfo, GeneralClientCapabilities, PositionEncodingKind,
        TextDocumentItem,
    };
    use serde_json::json;

    use super::*;
    use crate::documents::Documents;

    fn uri(spelling: &str) -> Uri {
        Uri::from_str(spelling).expect("the test URI parses")
    }

    fn folder(u: &str, name: &str) -> WorkspaceFolder {
        WorkspaceFolder {
            uri: uri(u),
            name: name.to_string(),
        }
    }

    #[allow(deprecated)] // `root_uri` is deprecated in LSP but still the input we echo.
    fn params_with_root(root: Option<&str>) -> InitializeParams {
        InitializeParams {
            root_uri: root.map(uri),
            ..InitializeParams::default()
        }
    }

    #[test]
    #[allow(deprecated)]
    fn from_params_stores_the_complete_client_supplied_snapshot() {
        let params = InitializeParams {
            client_info: Some(ClientInfo {
                name: "vscode".to_string(),
                version: Some("1.100.0".to_string()),
            }),
            capabilities: ClientCapabilities {
                general: Some(GeneralClientCapabilities {
                    position_encodings: Some(vec![PositionEncodingKind::UTF8]),
                    ..GeneralClientCapabilities::default()
                }),
                ..ClientCapabilities::default()
            },
            initialization_options: Some(json!({ "settings": { "tabSize": 4 } })),
            root_uri: Some(uri("file:///workspace/root")),
            workspace_folders: Some(vec![
                folder("file:///b", "second"),
                folder("file:///a", "first"),
            ]),
            ..InitializeParams::default()
        };

        let workspace = Workspace::from_params(&params, Documents::new());

        let client_info = workspace.client_info().expect("client info is stored");
        assert_eq!(client_info.name, "vscode");
        assert_eq!(client_info.version.as_deref(), Some("1.100.0"));
        assert_eq!(
            workspace.capabilities(),
            &params.capabilities,
            "client capabilities are stored verbatim"
        );
        assert_eq!(
            workspace.initialization_options(),
            params.initialization_options.as_ref(),
            "initialization options are stored verbatim"
        );
        assert_eq!(workspace.root_uri(), params.root_uri.as_ref());
        let names: Vec<String> = workspace.folders().into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["second", "first"], "folder order is preserved");
    }

    #[test]
    #[allow(deprecated)]
    fn roots_prefers_the_announced_folders_in_order() {
        let params = InitializeParams {
            root_uri: Some(uri("file:///workspace/root")),
            workspace_folders: Some(vec![folder("file:///b", "b"), folder("file:///a", "a")]),
            ..InitializeParams::default()
        };
        let workspace = Workspace::from_params(&params, Documents::new());

        assert_eq!(workspace.roots(), params.workspace_folders.unwrap());
    }

    #[test]
    fn roots_falls_back_to_one_synthetic_root_from_root_uri() {
        let workspace = Workspace::from_params(
            &params_with_root(Some("file:///workspace/root")),
            Documents::new(),
        );

        assert_eq!(
            workspace.roots(),
            vec![folder("file:///workspace/root", "root")],
            "the synthetic root takes the root URI and its final path segment"
        );
    }

    #[test]
    fn the_synthetic_root_is_named_workspace_without_a_final_path_segment() {
        let workspace =
            Workspace::from_params(&params_with_root(Some("file:///")), Documents::new());
        assert_eq!(workspace.roots(), vec![folder("file:///", "workspace")]);

        let workspace =
            Workspace::from_params(&params_with_root(Some("file:///c:/")), Documents::new());
        assert_eq!(
            workspace.roots(),
            vec![folder("file:///c:/", "c:")],
            "a drive root still has a final segment"
        );
    }

    #[test]
    #[allow(deprecated)]
    fn an_empty_folder_list_falls_back_to_root_uri() {
        let params = InitializeParams {
            root_uri: Some(uri("file:///solo")),
            workspace_folders: Some(vec![]),
            ..InitializeParams::default()
        };
        let workspace = Workspace::from_params(&params, Documents::new());

        assert_eq!(workspace.roots(), vec![folder("file:///solo", "solo")]);
    }

    #[test]
    fn no_root_uri_and_no_folders_yields_no_roots() {
        let workspace = Workspace::from_params(&InitializeParams::default(), Documents::new());

        assert!(workspace.roots().is_empty());
        assert_eq!(workspace.root_uri(), None);
        assert!(workspace.folders().is_empty());
    }

    #[test]
    fn the_synthetic_root_name_decodes_percent_encoding() {
        let workspace = Workspace::from_params(
            &params_with_root(Some("file:///Foo%20Bar")),
            Documents::new(),
        );

        assert_eq!(
            workspace.roots(),
            vec![folder("file:///Foo%20Bar", "Foo Bar")],
            "the name is a display string, so it decodes; the URI stays original"
        );
    }

    #[test]
    #[allow(deprecated)]
    fn public_values_retain_the_original_client_uris() {
        let params = InitializeParams {
            root_uri: Some(uri("file:///C%3A/Foo%20Bar")),
            workspace_folders: Some(vec![folder("FILE:///C%3A/Foo%20Bar", "orig")]),
            ..InitializeParams::default()
        };
        let workspace = Workspace::from_params(&params, Documents::new());

        assert_eq!(
            workspace.folders()[0].uri.as_str(),
            "FILE:///C%3A/Foo%20Bar"
        );
        assert_eq!(workspace.roots()[0].uri.as_str(), "FILE:///C%3A/Foo%20Bar");
        assert_eq!(
            workspace.root_uri().unwrap().as_str(),
            "file:///C%3A/Foo%20Bar",
            "no normalization leaks into the public values"
        );
    }

    #[test]
    fn clones_share_the_connection_documents() {
        let documents = Documents::new();
        let workspace = Workspace::from_params(&InitializeParams::default(), documents.clone());
        let clone = workspace.clone();

        documents.open(TextDocumentItem {
            uri: uri("file:///shared.rs"),
            language_id: "rust".to_string(),
            version: 1,
            text: "fn main() {}".to_string(),
        });

        for observer in [&workspace, &clone] {
            let doc = observer
                .documents()
                .get(&uri("file:///shared.rs"))
                .expect("a clone reads the same connection documents");
            assert_eq!(doc.text(), "fn main() {}");
        }
    }

    #[test]
    fn concurrent_clone_reads_are_safe_and_observe_later_folder_mutation() {
        let params = InitializeParams {
            workspace_folders: Some(vec![folder("file:///before", "before")]),
            ..InitializeParams::default()
        };
        let workspace = Workspace::from_params(&params, Documents::new());
        let barrier = Arc::new(Barrier::new(5));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let clone = workspace.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..100 {
                        let _snapshot = clone.folders();
                    }
                })
            })
            .collect();

        barrier.wait();
        workspace.apply_folder_change(DidChangeWorkspaceFoldersParams {
            event: lsp_types::WorkspaceFoldersChangeEvent {
                removed: vec![folder("file:///before", "ignored")],
                added: vec![folder("file:///after", "after")],
            },
        });
        for reader in readers {
            reader.join().expect("concurrent reader did not panic");
        }

        assert_eq!(
            workspace.clone().folders(),
            vec![folder("file:///after", "after")],
            "a cloned handle observes mutation made through the shared workspace"
        );
    }
}
