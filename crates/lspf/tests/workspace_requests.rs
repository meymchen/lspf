//! End-to-end coverage for workspace requests and file-operation routes
//! (issue #73).
//!
//! Registering `features::workspace_symbol(options)` and
//! `features::workspace_symbol_resolve()` routes both `workspace/symbol` and
//! `workspaceSymbol/resolve` with typed values, while one family-aware merge
//! emits a single `workspaceSymbolProvider` capability. The file-operation
//! descriptors route the `will*` requests and `did*` notifications and share
//! one registration-options value per operation family, emitting one
//! deterministic `workspace.fileOperations` capability — verified
//! byte-for-byte against `fixtures/workspace_file_operations.json`. Static
//! and initialize-conditional registrations share the same merge and
//! validation rules, so a conditional contribution whose filters drift from
//! its family fails the initialize transaction exactly as a static one fails
//! `build`.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use tokio::sync::mpsc;

use lspf::types::notification::DidDeleteFiles;
use lspf::types::{
    CreateFilesParams, DeleteFilesParams, DidChangeWatchedFilesParams, FileChangeType,
    FileOperationFilter, FileOperationPattern, FileOperationPatternKind,
    FileOperationRegistrationOptions, RenameFilesParams, WorkspaceEdit, WorkspaceSymbol,
    WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use lspf::{
    CancellationToken, LspError, RawMessage, RequestId, Server, ServerContext, Transport,
    TransportError, TransportReader, TransportWriter,
};

/// Application state shared as `Arc<S>` by every handler on the connection.
/// Each vector records the typed values its handler observed, in order.
#[derive(Default)]
struct AppState {
    symbol_queries: Arc<Mutex<Vec<String>>>,
    renames: Arc<Mutex<Vec<(String, String)>>>,
    creates: Arc<Mutex<Vec<String>>>,
    deletes: Arc<Mutex<Vec<String>>>,
    watched_changes: Arc<Mutex<Vec<FileChangeType>>>,
}

async fn workspace_symbol(
    state: Arc<AppState>,
    _ctx: ServerContext,
    params: WorkspaceSymbolParams,
    _ct: CancellationToken,
) -> Result<Option<WorkspaceSymbolResponse>, LspError> {
    state.symbol_queries.lock().unwrap().push(params.query);
    Ok(Some(WorkspaceSymbolResponse::WorkspaceSymbolList(vec![])))
}

async fn symbol_resolve(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    mut symbol: WorkspaceSymbol,
    _ct: CancellationToken,
) -> Result<WorkspaceSymbol, LspError> {
    symbol.base_symbol_information.container_name = Some("resolved container".to_string());
    Ok(symbol)
}

async fn will_create(
    state: Arc<AppState>,
    _ctx: ServerContext,
    params: CreateFilesParams,
    _ct: CancellationToken,
) -> Result<Option<WorkspaceEdit>, LspError> {
    let mut creates = state.creates.lock().unwrap();
    for file in params.files {
        creates.push(file.uri.to_string());
    }
    Ok(Some(WorkspaceEdit::default()))
}

async fn will_rename(
    state: Arc<AppState>,
    _ctx: ServerContext,
    params: RenameFilesParams,
    _ct: CancellationToken,
) -> Result<Option<WorkspaceEdit>, LspError> {
    let mut renames = state.renames.lock().unwrap();
    for file in params.files {
        renames.push((file.old_uri.to_string(), file.new_uri.to_string()));
    }
    Ok(Some(WorkspaceEdit::default()))
}

async fn will_delete(
    state: Arc<AppState>,
    _ctx: ServerContext,
    params: DeleteFilesParams,
    _ct: CancellationToken,
) -> Result<Option<WorkspaceEdit>, LspError> {
    let mut deletes = state.deletes.lock().unwrap();
    for file in params.files {
        deletes.push(file.uri.to_string());
    }
    Ok(Some(WorkspaceEdit::default()))
}

async fn did_rename(state: Arc<AppState>, _ctx: ServerContext, params: RenameFilesParams) {
    let mut renames = state.renames.lock().unwrap();
    for file in params.files {
        renames.push((file.old_uri.to_string(), file.new_uri.to_string()));
    }
}

async fn did_create(state: Arc<AppState>, _ctx: ServerContext, params: CreateFilesParams) {
    let mut creates = state.creates.lock().unwrap();
    for file in params.files {
        creates.push(file.uri.to_string());
    }
}

async fn did_delete(state: Arc<AppState>, _ctx: ServerContext, params: DeleteFilesParams) {
    let mut deletes = state.deletes.lock().unwrap();
    for file in params.files {
        deletes.push(file.uri.to_string());
    }
}

async fn did_change_watched(
    state: Arc<AppState>,
    _ctx: ServerContext,
    params: DidChangeWatchedFilesParams,
) {
    let mut watched = state.watched_changes.lock().unwrap();
    for change in params.changes {
        watched.push(change.kind);
    }
}

fn rust_file_filters() -> FileOperationRegistrationOptions {
    FileOperationRegistrationOptions {
        filters: vec![FileOperationFilter {
            scheme: Some("file".to_string()),
            pattern: FileOperationPattern {
                glob: "**/*.rs".to_string(),
                matches: Some(FileOperationPatternKind::File),
                options: None,
            },
        }],
    }
}

// --- In-memory transport -----------------------------------------------------

struct ChannelTransport {
    in_rx: mpsc::UnboundedReceiver<RawMessage>,
    out_tx: mpsc::UnboundedSender<RawMessage>,
}

struct ChannelReader {
    in_rx: mpsc::UnboundedReceiver<RawMessage>,
}

struct ChannelWriter {
    out_tx: mpsc::UnboundedSender<RawMessage>,
}

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            ChannelReader { in_rx: self.in_rx },
            ChannelWriter {
                out_tx: self.out_tx,
            },
        )
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.in_rx.recv().await.ok_or(TransportError::Closed)
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, msg: RawMessage) -> Result<(), TransportError> {
        self.out_tx.send(msg).map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

// --- Envelope helpers --------------------------------------------------------

fn request(id: i32, method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn notification(method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn initialize_request(id: i32) -> RawMessage {
    request(
        id,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    )
}

fn exit() -> RawMessage {
    notification("exit", json!(null))
}

/// Drive `server` with `messages`, then close the transport so `serve` returns
/// once everything is processed. Returns the outbox.
async fn drive(server: Server<AppState>, messages: Vec<RawMessage>) -> Vec<RawMessage> {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let transport = ChannelTransport { in_rx, out_tx };

    let mut handle = tokio::spawn(async move { server.serve(transport).await });
    let mut server_done = false;
    let mut outbox = Vec::new();

    'messages: for msg in messages {
        let response_id = msg.id().cloned();
        // A failed initialize transaction terminates the connection, so a send
        // can legitimately race the disconnect; stop feeding the channel
        // instead of panicking on `SendError`.
        if in_tx.send(msg).is_err() {
            break;
        }
        if let Some(response_id) = response_id {
            tokio::select! {
                response = out_rx.recv() => {
                    if let Some(response) = response {
                        assert_eq!(response.id(), Some(&response_id));
                        outbox.push(response);
                    } else {
                        (&mut handle)
                            .await
                            .expect("server task did not panic")
                            .expect("serve ended cleanly");
                        server_done = true;
                        break 'messages;
                    }
                }
                result = &mut handle => {
                    result
                        .expect("server task did not panic")
                        .expect("serve ended cleanly");
                    server_done = true;
                    break 'messages;
                }
            }
        }
    }
    drop(in_tx);

    if !server_done {
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("serve returned within 2s")
            .expect("server task did not panic")
            .expect("serve ended cleanly");
    }

    outbox.extend(std::iter::from_fn(|| out_rx.try_recv().ok()));
    outbox
}

fn response(outbox: &[RawMessage], id: i32) -> Option<&RawMessage> {
    outbox.iter().find(
        |m| matches!(m, RawMessage::Response { id: rid, .. } if *rid == RequestId::Number(id)),
    )
}

fn ok_result(outbox: &[RawMessage], id: i32) -> Option<serde_json::Value> {
    match response(outbox, id)? {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => Some(serde_json::from_slice(bytes).unwrap()),
        _ => None,
    }
}

fn error_code(outbox: &[RawMessage], id: i32) -> Option<i32> {
    match response(outbox, id)? {
        RawMessage::Response { result: Err(e), .. } => Some(e.code),
        _ => None,
    }
}

fn initialize_capabilities(outbox: &[RawMessage], id: i32) -> serde_json::Value {
    ok_result(outbox, id).expect("initialize response")["capabilities"].clone()
}

// --- Tests -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_symbol_and_resolve_dispatch_typed_values_and_merge_one_capability() {
    let state = AppState::default();
    let symbol_queries = Arc::clone(&state.symbol_queries);
    let server = Server::builder(state)
        .feature(
            lspf::features::workspace_symbol(lspf::types::WorkspaceSymbolOptions {
                work_done_progress_options: Default::default(),
                resolve_provider: None,
            }),
            workspace_symbol,
        )
        .feature(lspf::features::workspace_symbol_resolve(), symbol_resolve)
        .build()
        .expect("workspace symbol and resolve build");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            request(2, "workspace/symbol", json!({ "query": "main" })),
            request(
                3,
                "workspaceSymbol/resolve",
                json!({
                    "name": "main",
                    "kind": 12,
                    "location": { "uri": "file:///src/main.rs" }
                }),
            ),
            exit(),
        ],
    )
    .await;

    // The family merge emits one capability carrying both contributions.
    let capabilities = initialize_capabilities(&outbox, 1);
    assert_eq!(
        capabilities["workspaceSymbolProvider"],
        json!({ "resolveProvider": true }),
        "the resolve contribution augments the base capability with resolveProvider"
    );

    // Both routes dispatch typed values.
    assert_eq!(*symbol_queries.lock().unwrap(), vec!["main".to_string()]);
    assert_eq!(
        ok_result(&outbox, 2).expect("workspace/symbol response"),
        json!([]),
        "the untagged protocol union serializes an empty symbol list as an empty array"
    );
    let resolved: WorkspaceSymbol =
        serde_json::from_value(ok_result(&outbox, 3).expect("resolve response")).unwrap();
    assert_eq!(resolved.base_symbol_information.name, "main");
    assert_eq!(
        resolved.base_symbol_information.container_name.as_deref(),
        Some("resolved container")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_operation_routes_dispatch_typed_values_and_merge_one_capability() {
    let state = AppState::default();
    let renames = Arc::clone(&state.renames);
    let creates = Arc::clone(&state.creates);
    let deletes = Arc::clone(&state.deletes);
    let watched_changes = Arc::clone(&state.watched_changes);
    let server = Server::builder(state)
        .feature(
            lspf::features::will_create_files(rust_file_filters()),
            will_create,
        )
        .feature(
            lspf::features::will_rename_files(rust_file_filters()),
            will_rename,
        )
        .feature(
            lspf::features::will_delete_files(rust_file_filters()),
            will_delete,
        )
        .feature_notification(lspf::features::did_rename_files(rust_file_filters()), did_rename)
        .feature_notification(lspf::features::did_create_files(rust_file_filters()), did_create)
        .feature_notification(lspf::features::did_change_watched_files(), did_change_watched)
        // The same notification method is also registrable as a plain typed
        // notification; it then advertises no capability of its own.
        .notification::<DidDeleteFiles, _, _>(did_delete)
        .build()
        .expect("file-operation routes build");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            request(
                2,
                "workspace/willRenameFiles",
                json!({ "files": [
                    { "oldUri": "file:///old.rs", "newUri": "file:///new.rs" }
                ] }),
            ),
            request(
                3,
                "workspace/willCreateFiles",
                json!({ "files": [ { "uri": "file:///planned.rs" } ] }),
            ),
            request(
                4,
                "workspace/willDeleteFiles",
                json!({ "files": [ { "uri": "file:///doomed.rs" } ] }),
            ),
            notification(
                "workspace/didRenameFiles",
                json!({ "files": [
                    { "oldUri": "file:///a.rs", "newUri": "file:///b.rs" }
                ] }),
            ),
            notification(
                "workspace/didCreateFiles",
                json!({ "files": [ { "uri": "file:///created.rs" } ] }),
            ),
            notification(
                "workspace/didDeleteFiles",
                json!({ "files": [ { "uri": "file:///deleted.rs" } ] }),
            ),
            notification(
                "workspace/didChangeWatchedFiles",
                json!({ "changes": [ { "uri": "file:///created.rs", "type": 2 } ] }),
            ),
            exit(),
        ],
    )
    .await;

    // Every route dispatched its typed values.
    assert_eq!(
        *renames.lock().unwrap(),
        vec![
            ("file:///old.rs".to_string(), "file:///new.rs".to_string()),
            ("file:///a.rs".to_string(), "file:///b.rs".to_string()),
        ],
        "the will* request and the did* notification both reached their handlers"
    );
    assert_eq!(
        *creates.lock().unwrap(),
        vec![
            "file:///planned.rs".to_string(),
            "file:///created.rs".to_string()
        ]
    );
    assert_eq!(
        *deletes.lock().unwrap(),
        vec![
            "file:///doomed.rs".to_string(),
            "file:///deleted.rs".to_string()
        ]
    );
    assert_eq!(
        *watched_changes.lock().unwrap(),
        vec![FileChangeType::Changed]
    );

    // Every will* request returned its encoded WorkspaceEdit.
    for id in [2, 3, 4] {
        assert_eq!(
            ok_result(&outbox, id).unwrap_or_else(|| panic!("request {id} was answered")),
            json!({})
        );
    }

    // One deterministic fileOperations capability: each family's two sides
    // share the family's filters, the create family advertises only its
    // registered side, and the plain didDeleteFiles route advertises nothing.
    let file_operations = &initialize_capabilities(&outbox, 1)["workspace"]["fileOperations"];
    let filters = json!({ "filters": [
        { "scheme": "file", "pattern": { "glob": "**/*.rs", "matches": "file" } }
    ] });
    assert_eq!(file_operations["willCreate"], filters.clone());
    assert_eq!(file_operations["willRename"], filters.clone());
    assert_eq!(file_operations["willDelete"], filters.clone());
    assert_eq!(file_operations["didRename"], filters.clone());
    assert_eq!(file_operations["didCreate"], filters);
    assert!(
        file_operations.get("didDelete").is_none(),
        "a plain typed notification contributes no capability"
    );
    // Watched files have no server capability field in LSP 3.17.
    let workspace = &initialize_capabilities(&outbox, 1)["workspace"];
    assert!(workspace.get("didChangeWatchedFiles").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_file_operation_registration_uses_the_same_merge() {
    let server = Server::builder(AppState::default())
        .feature(
            lspf::features::will_rename_files(rust_file_filters()),
            will_rename,
        )
        .configure_initialize(|_params, registrar| {
            registrar.feature_notification(
                lspf::features::did_rename_files(rust_file_filters()),
                did_rename,
            );
            Ok(())
        })
        .build()
        .expect("a static will with a conditional did builds");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;

    let file_operations = &initialize_capabilities(&outbox, 1)["workspace"]["fileOperations"];
    assert_eq!(
        file_operations["willRename"], file_operations["didRename"],
        "conditional registrations merge through the same family rules"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_filters_drifting_from_their_family_fail_initialization() {
    let drifting = FileOperationRegistrationOptions {
        filters: vec![FileOperationFilter {
            scheme: Some("file".to_string()),
            pattern: FileOperationPattern {
                glob: "**/*.toml".to_string(),
                matches: Some(FileOperationPatternKind::File),
                options: None,
            },
        }],
    };
    let server = Server::builder(AppState::default())
        .feature(
            lspf::features::will_rename_files(rust_file_filters()),
            will_rename,
        )
        .configure_initialize(move |_params, registrar| {
            registrar.feature_notification(lspf::features::did_rename_files(drifting), did_rename);
            Ok(())
        })
        .build()
        .expect("build does not run the conditional transaction");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;

    assert_eq!(
        error_code(&outbox, 1),
        Some(-32603),
        "combined validation rejects drifting family filters with InternalError"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merged_file_operations_capability_is_byte_stable() {
    let server = Server::builder(AppState::default())
        .feature(
            lspf::features::will_rename_files(rust_file_filters()),
            will_rename,
        )
        .feature_notification(
            lspf::features::did_rename_files(rust_file_filters()),
            did_rename,
        )
        .feature_notification(
            lspf::features::did_create_files(rust_file_filters()),
            did_create,
        )
        .build()
        .expect("file-operation routes build");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;

    // Compare against the raw wire bytes, not a re-serialized typed value, so
    // an added, renamed, or reordered field in the emitted object breaks here.
    let wire = match response(&outbox, 1).expect("initialize response") {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => String::from_utf8(bytes.to_vec()).unwrap(),
        other => panic!("expected a successful initialize response, got {other:?}"),
    };
    let fixture = include_str!("fixtures/workspace_file_operations.json").trim_end();
    assert!(
        wire.contains(&format!("\"fileOperations\":{fixture}")),
        "the merged fileOperations capability on the wire must stay byte-stable; \
         update the fixture only with a deliberate capability change.\nwire: {wire}"
    );
    // The protocol-owned workspaceFolders capability survives beside the
    // registration-contributed fileOperations.
    assert!(
        wire.contains("\"workspaceFolders\":{\"supported\":true"),
        "the engine layers workspaceFolders beside fileOperations\nwire: {wire}"
    );
}
