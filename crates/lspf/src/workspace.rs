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

use crate::documents::{Document, Documents, DocumentsView};
use crate::file_provider::SharedFileProvider;
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

struct WorkspaceState {
    client_info: Option<ClientInfo>,
    capabilities: ClientCapabilities,
    initialization_options: Option<Value>,
    root_uri: Option<Uri>,
    folders: RwLock<Vec<WorkspaceFolder>>,
    configuration: RwLock<Option<Value>>,
    trace: SharedTrace,
    documents: Documents,
    file_provider: SharedFileProvider,
}

/// The connection's protocol trace level, shared between the [`Workspace`]
/// and the connection's [`Client`](crate::Client).
///
/// `$/setTrace` writes through the workspace; [`Client::log_trace`](crate::Client::log_trace)
/// reads the same cell to gate its enqueue, so the two never observe
/// different levels.
#[derive(Debug, Clone, Default)]
pub(crate) struct SharedTrace(Arc<RwLock<TraceValue>>);

impl SharedTrace {
    pub(crate) fn get(&self) -> TraceValue {
        *self.0.read().unwrap()
    }

    fn set(&self, value: TraceValue) {
        *self.0.write().unwrap() = value;
    }
}

/// Failure to resolve a document through the connection's workspace.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// No open document and the configured provider has no resource for the
    /// URI.
    #[error("resource not found")]
    NotFound,

    /// The resource's scheme is not served by the configured provider.
    #[error("scheme `{0}` is not supported")]
    UnsupportedScheme(String),

    /// The resource's path or contents are not valid UTF-8.
    #[error("the resource path or contents are not valid UTF-8")]
    InvalidEncoding,

    /// The resource is larger than the provider's configured byte limit.
    #[error("the resource exceeds the maximum read size of {limit} bytes")]
    TooLarge {
        /// Configured maximum number of bytes.
        limit: u64,
    },

    /// The provider failed to read the resource.
    #[error("failed to read the resource: {0}")]
    Io(#[from] std::io::Error),
}

impl std::fmt::Debug for WorkspaceState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceState")
            .field("client_info", &self.client_info)
            .field("capabilities", &self.capabilities)
            .field("initialization_options", &self.initialization_options)
            .field("root_uri", &self.root_uri)
            .field("folders", &self.folders)
            .field("configuration", &self.configuration)
            .field("trace", &self.trace)
            .field("documents", &self.documents)
            .field("file_provider", &"<FileProvider>")
            .finish()
    }
}

impl Workspace {
    /// Establish the workspace from the client's `InitializeParams` (ADR 0018).
    ///
    /// Captures the client info, capabilities, initialization options, the
    /// (possibly deprecated) `rootUri`, and the announced workspace folders —
    /// all verbatim, folder order included; an absent `workspaceFolders` list
    /// yields no folders. `documents` is the connection's document store,
    /// which the workspace owns from here on.
    ///
    /// This test-only constructor gives the workspace a private trace cell;
    /// the engine uses [`Workspace::from_params_with_provider`] so the cell
    /// is the one the connection's [`Client`](crate::Client) reads.
    #[cfg(test)]
    pub(crate) fn from_params(params: &InitializeParams, documents: Documents) -> Self {
        Self::from_params_with_provider(
            params,
            documents,
            crate::file_provider::erase(crate::MemoryFileProvider::new()),
            SharedTrace::default(),
        )
    }

    pub(crate) fn from_params_with_provider(
        params: &InitializeParams,
        documents: Documents,
        file_provider: SharedFileProvider,
        trace: SharedTrace,
    ) -> Self {
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
                trace,
                documents,
                file_provider,
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
        self.inner.trace.get()
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

    /// Resolve a document snapshot, preferring editor-open text over the
    /// configured provider for unopened resources.
    pub async fn text_document(&self, uri: &Uri) -> Result<Document, WorkspaceError> {
        if let Some(document) = self.inner.documents.get(uri) {
            return Ok(document);
        }
        match self.inner.file_provider.read_text(uri).await {
            Ok(Some(text)) => Ok(Document::provider_snapshot(uri.clone(), text)),
            Ok(None) => Err(WorkspaceError::NotFound),
            Err(error) => Err(error),
        }
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
                existing.name = added.name;
            } else {
                folders.push(added);
            }
        }
    }

    pub(crate) fn set_configuration(&self, settings: Value) {
        *self.inner.configuration.write().unwrap() = Some(settings);
    }

    pub(crate) fn set_trace(&self, trace: TraceValue) {
        self.inner.trace.set(trace);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::str::FromStr;
    use std::sync::Barrier;

    use lsp_types::{
        ClientCapabilities, ClientInfo, GeneralClientCapabilities, PositionEncodingKind,
        TextDocumentItem,
    };
    use serde_json::json;

    use super::*;
    #[cfg(feature = "runtime-tokio")]
    use crate::OsFileProvider;
    use crate::documents::Documents;
    use crate::file_provider::erase;
    #[cfg(feature = "runtime-tokio")]
    use crate::test_util::file_uri;
    use crate::{MemoryFileProvider, WorkspaceError};

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
    fn roots_preserve_non_file_workspace_folders_in_order() {
        // Unlike gopls, the framework has no language-specific view policy:
        // workspace folder URIs are retained verbatim regardless of scheme.
        let announced = vec![
            folder("virtual:///generated", "generated"),
            folder("file:///source", "source"),
            folder("untitled:workspace", "scratch"),
        ];
        let params = InitializeParams {
            workspace_folders: Some(announced.clone()),
            ..InitializeParams::default()
        };
        let workspace = Workspace::from_params(&params, Documents::new());

        assert_eq!(workspace.folders(), announced);
        assert_eq!(workspace.roots(), announced);
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

        documents
            .open(TextDocumentItem {
                uri: uri("file:///shared.rs"),
                language_id: "rust".to_string(),
                version: 1,
                text: "fn main() {}".to_string(),
            })
            .expect("the default policy accepts the test document");

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

    #[tokio::test]
    async fn text_document_prefers_an_open_document_over_the_provider() {
        let documents = Documents::new();
        let provider = MemoryFileProvider::new();
        let requested = uri("file:///workspace/main.rs");
        provider.insert(requested.clone(), "provider");
        documents
            .open(TextDocumentItem {
                uri: requested.clone(),
                language_id: "rust".to_string(),
                version: 7,
                text: "editor".to_string(),
            })
            .expect("the default policy accepts the test document");
        let workspace = Workspace::from_params_with_provider(
            &InitializeParams::default(),
            documents,
            erase(provider),
            SharedTrace::default(),
        );

        let document = workspace.text_document(&requested).await.unwrap();

        assert_eq!(document.text(), "editor");
        assert_eq!(document.version(), Some(7));
    }

    #[tokio::test]
    async fn provider_snapshots_are_versionless_not_cached_and_not_opened() {
        let documents = Documents::new();
        let provider = MemoryFileProvider::new();
        let inserted = uri("file:///workspace/%61.rs");
        let requested = uri("FILE:///workspace/a.rs");
        provider.insert(inserted.clone(), "first");
        let workspace = Workspace::from_params_with_provider(
            &InitializeParams::default(),
            documents.clone(),
            erase(provider.clone()),
            SharedTrace::default(),
        );

        let first = workspace.text_document(&requested).await.unwrap();
        assert_eq!(first.uri(), &requested);
        assert_eq!(first.text(), "first");
        assert_eq!(first.version(), None);
        assert!(documents.get(&requested).is_none());

        provider.insert(inserted, "second");
        assert_eq!(
            workspace.text_document(&requested).await.unwrap().text(),
            "second",
            "an unopened lookup consults the provider every time"
        );
    }

    #[tokio::test]
    async fn a_missing_provider_resource_is_not_found() {
        let requested = uri("file:///workspace/missing.rs");
        let workspace = Workspace::from_params_with_provider(
            &InitializeParams::default(),
            Documents::new(),
            erase(MemoryFileProvider::new()),
            SharedTrace::default(),
        );

        assert!(matches!(
            workspace.text_document(&requested).await,
            Err(WorkspaceError::NotFound)
        ));
    }

    #[tokio::test]
    #[cfg(feature = "runtime-tokio")]
    async fn os_provider_snapshots_are_versionless_not_cached_and_not_opened() {
        let dir = tempfile::tempdir().expect("a tempdir is created");
        let file = dir.path().join("provider.rs");
        std::fs::write(&file, "provider one").expect("the test file writes");
        let requested = file_uri(&file);
        let documents = Documents::new();
        let workspace = Workspace::from_params_with_provider(
            &InitializeParams::default(),
            documents.clone(),
            erase(OsFileProvider::new()),
            SharedTrace::default(),
        );

        let first = workspace.text_document(&requested).await.unwrap();
        assert_eq!(first.uri(), &requested);
        assert_eq!(first.text(), "provider one");
        assert_eq!(first.version(), None);
        assert!(
            documents.get(&requested).is_none(),
            "a provider read never becomes an open document"
        );

        std::fs::write(&file, "provider two").expect("the test file rewrites");
        assert_eq!(
            workspace.text_document(&requested).await.unwrap().text(),
            "provider two",
            "an unopened lookup reads the filesystem every time"
        );
    }

    #[tokio::test]
    #[cfg(feature = "runtime-tokio")]
    async fn os_provider_reads_files_outside_every_workspace_root() {
        let dir = tempfile::tempdir().expect("a tempdir is created");
        let file = dir.path().join("outside.rs");
        std::fs::write(&file, "outside the root").expect("the test file writes");
        let requested = file_uri(&file);
        let workspace = Workspace::from_params_with_provider(
            &params_with_root(Some("file:///workspace/root")),
            Documents::new(),
            erase(OsFileProvider::new()),
            SharedTrace::default(),
        );

        assert_eq!(
            workspace.text_document(&requested).await.unwrap().text(),
            "outside the root"
        );
    }

    #[tokio::test]
    #[cfg(feature = "runtime-tokio")]
    async fn os_provider_missing_file_is_not_found() {
        let dir = tempfile::tempdir().expect("a tempdir is created");
        let requested = file_uri(&dir.path().join("absent.rs"));
        let workspace = Workspace::from_params_with_provider(
            &InitializeParams::default(),
            Documents::new(),
            erase(OsFileProvider::new()),
            SharedTrace::default(),
        );

        assert!(matches!(
            workspace.text_document(&requested).await,
            Err(WorkspaceError::NotFound)
        ));
    }

    #[tokio::test]
    #[cfg(feature = "runtime-tokio")]
    async fn os_provider_rejects_non_file_schemes() {
        let requested = uri("untitled:Untitled-1");
        let workspace = Workspace::from_params_with_provider(
            &InitializeParams::default(),
            Documents::new(),
            erase(OsFileProvider::new()),
            SharedTrace::default(),
        );

        assert!(matches!(
            workspace.text_document(&requested).await,
            Err(WorkspaceError::UnsupportedScheme(scheme)) if scheme == "untitled"
        ));
    }

    #[tokio::test]
    #[cfg(feature = "runtime-tokio")]
    async fn os_provider_io_failures_surface_through_text_document() {
        let dir = tempfile::tempdir().expect("a tempdir is created");
        let requested = file_uri(dir.path());
        let workspace = Workspace::from_params_with_provider(
            &InitializeParams::default(),
            Documents::new(),
            erase(OsFileProvider::new()),
            SharedTrace::default(),
        );

        assert!(matches!(
            workspace.text_document(&requested).await,
            Err(WorkspaceError::Io(_))
        ));
    }
}
