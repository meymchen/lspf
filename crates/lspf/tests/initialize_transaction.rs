//! End-to-end coverage for the 0.2 initialize transaction (issue #42).
//!
//! Initialization is the one bounded phase that can conditionally extend the
//! Router, freeze it, generate capabilities, establish the connection's
//! `Workspace`, `Documents`, and negotiated position encoding, and run the
//! `on_initialize` lifecycle hook — all without exposing partial state
//! (ADR 0017, ADR 0018). These tests drive real envelopes over an in-memory
//! channel-backed [`Transport`] and inspect the outbox.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

use lspf::types::request::Request;
use lspf::types::{InitializeResult, PositionEncodingKind, ServerCapabilities, ServerInfo, Uri};
use lspf::{
    Context, PositionEncoding, RawMessage, RequestId, Server, Transport, TransportError,
    TransportReader, TransportWriter,
};

// --- A conditional custom request marker -------------------------------------

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PingParams {
    value: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PingResult {
    echoed: String,
}

/// A marker registered only from `configure_initialize`, never statically.
enum Ping {}

impl Request for Ping {
    type Params = PingParams;
    type Result = PingResult;
    const METHOD: &'static str = "custom/ping";
}

async fn ping(
    _state: Arc<AppState>,
    _ctx: Context,
    params: PingParams,
    _ct: lspf::CancellationToken,
) -> Result<PingResult, lspf::LspError> {
    Ok(PingResult {
        echoed: params.value,
    })
}

// --- A document-probe request reading through the Context --------------------

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct DocProbeParams {
    uri: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct DocProbeResult {
    found: bool,
    opened_uri: Option<String>,
    text: Option<String>,
}

/// A marker whose handler reads `ctx.documents()`, so the test drives document
/// identity end to end over the wire.
enum DocProbe {}

impl Request for DocProbe {
    type Params = DocProbeParams;
    type Result = DocProbeResult;
    const METHOD: &'static str = "custom/docProbe";
}

async fn doc_probe(
    _state: Arc<AppState>,
    ctx: Context,
    params: DocProbeParams,
    _ct: lspf::CancellationToken,
) -> Result<DocProbeResult, lspf::LspError> {
    let probe = params.uri.parse::<Uri>().expect("the probe URI parses");
    let doc = ctx.documents().get(&probe);
    Ok(DocProbeResult {
        found: doc.is_some(),
        opened_uri: doc.as_ref().map(|d| d.uri().as_str().to_string()),
        text: doc.map(|d| d.text()),
    })
}

/// Application state shared across handlers. `observed` is an `Arc` so the test
/// keeps a handle after the state moves into the server.
#[derive(Clone, Default)]
struct AppState {
    /// What `on_initialize` observed of the established framework state.
    observed: Arc<Mutex<Option<Observed>>>,
}

#[derive(Clone)]
struct Observed {
    encoding: PositionEncoding,
    root_uri: Option<Uri>,
    folder_count: usize,
}

/// State for tests capturing the complete workspace snapshot `on_initialize`
/// observes. Fields not exercised by a test stay at their `Default`.
#[derive(Clone, Default)]
struct SnapshotState {
    observed: Arc<Mutex<Option<WorkspaceSnapshot>>>,
}

#[derive(Clone, Default)]
struct WorkspaceSnapshot {
    client_name: Option<String>,
    client_version: Option<String>,
    initialization_options: Option<serde_json::Value>,
    position_encodings: Option<Vec<PositionEncodingKind>>,
    root_uri: Option<Uri>,
    folder_names: Vec<String>,
    roots: Vec<(String, String)>,
}

fn roots_of(ctx: &Context) -> Vec<(String, String)> {
    ctx.workspace()
        .roots()
        .iter()
        .map(|folder| (folder.uri.as_str().to_string(), folder.name.clone()))
        .collect()
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

fn initialize_request(id: i32, params: serde_json::Value) -> RawMessage {
    request(id, "initialize", params)
}

/// The minimal `initialize` params: no client capabilities, no folders.
fn bare_initialize(id: i32) -> RawMessage {
    initialize_request(
        id,
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    )
}

fn notification(method: &'static str) -> RawMessage {
    notification_with(method, serde_json::Value::Null)
}

fn notification_with(method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

/// Drive `server` with `messages`, then close the transport so `serve` returns
/// once everything is processed. Returns the outbox.
async fn drive<S>(server: Server<S>, messages: Vec<RawMessage>) -> Vec<RawMessage>
where
    S: Send + Sync + 'static,
{
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let transport = ChannelTransport { in_rx, out_tx };

    let mut handle = tokio::spawn(async move { server.serve(transport).await });
    let mut server_done = false;
    let mut outbox = Vec::new();

    'messages: for msg in messages {
        let response_id = msg.id().cloned();
        // The close-path tests make the server terminate mid-stream (a failed
        // initialize drops the reader), so a send can legitimately race the
        // disconnect. Treat a closed channel like a real transport would —
        // stop feeding it — rather than panicking on `SendError`.
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
    drop(in_tx); // peer disconnect → serve drains and returns

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

fn initialize_result(outbox: &[RawMessage], id: i32) -> InitializeResult {
    serde_json::from_value(ok_result(outbox, id).expect("initialize response")).unwrap()
}

// --- Tests -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configure_initialize_registers_a_conditional_route_that_dispatches() {
    // A route registered only from `configure_initialize` is committed to the
    // frozen Router and dispatches like any other; the callback runs once.
    let server = Server::builder(AppState::default())
        .configure_initialize(|_params, registrar| {
            registrar.request::<Ping, _, _>(ping);
            Ok(())
        })
        .build()
        .expect("server builds");

    let outbox = drive(
        server,
        vec![
            bare_initialize(1),
            request(2, "custom/ping", json!({ "value": "pong" })),
            request(3, "shutdown", json!(null)),
            notification("exit"),
        ],
    )
    .await;

    // initialize succeeded.
    assert!(ok_result(&outbox, 1).is_some(), "initialize succeeds");

    // The conditionally-registered route decoded, ran, and encoded a result.
    let ping: PingResult =
        serde_json::from_value(ok_result(&outbox, 2).expect("ping response")).unwrap();
    assert_eq!(ping.echoed, "pong");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configure_initialize_runs_exactly_once_after_a_valid_initialize() {
    let ran = Arc::new(AtomicUsize::new(0));
    let ran_in_cb = Arc::clone(&ran);
    let server = Server::builder(AppState::default())
        .configure_initialize(move |_params, _registrar| {
            ran_in_cb.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .build()
        .expect("server builds");

    // A malformed initialize (missing `capabilities`) must not spend the
    // transaction; a following valid initialize then runs it exactly once.
    let outbox = drive(
        server,
        vec![
            initialize_request(1, json!({ "not": "valid initialize params" })),
            bare_initialize(2),
            bare_initialize(3),
            notification("exit"),
        ],
    )
    .await;

    assert!(
        error_code(&outbox, 1).is_some(),
        "the malformed initialize is rejected"
    );
    assert!(
        ok_result(&outbox, 2).is_some(),
        "the first valid initialize succeeds"
    );
    assert_eq!(
        error_code(&outbox, 3),
        Some(-32600),
        "a second initialize is refused"
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "configure_initialize runs exactly once, only after a valid initialize"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configure_initialize_can_advertise_a_conditional_feature() {
    // A feature registered only from the transaction contributes to the
    // capabilities generated from the frozen Router.
    let server = Server::builder(AppState::default())
        .configure_initialize(|_params, registrar| {
            registrar.feature(lspf::features::hover(), hover);
            Ok(())
        })
        .build()
        .expect("server builds");

    let outbox = drive(server, vec![bare_initialize(1), notification("exit")]).await;

    let init = initialize_result(&outbox, 1);
    assert_eq!(
        init.capabilities.hover_provider,
        Some(lspf::types::HoverProviderCapability::Simple(true)),
        "a conditionally-registered feature is advertised from the frozen catalog"
    );
}

async fn hover(
    _state: Arc<AppState>,
    _ctx: Context,
    _params: lspf::types::HoverParams,
    _ct: lspf::CancellationToken,
) -> Result<Option<lspf::types::Hover>, lspf::LspError> {
    Ok(None)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_configure_callback_sends_internal_error_and_exposes_no_route() {
    // When the callback returns Err, the whole transaction is discarded: the
    // request gets InternalError and no conditional route becomes observable.
    let server = Server::builder(AppState::default())
        .configure_initialize(|_params, registrar| {
            // Register a route, then fail — it must not survive.
            registrar.request::<Ping, _, _>(ping);
            Err(lspf::LspError::internal("conditional setup failed"))
        })
        .build()
        .expect("server builds");

    let outbox = drive(
        server,
        vec![
            bare_initialize(1),
            // The connection enters the close path after the failed initialize,
            // so this request is never answered; its absence is asserted below.
            request(2, "custom/ping", json!({ "value": "pong" })),
            notification("exit"),
        ],
    )
    .await;

    assert_eq!(
        error_code(&outbox, 1),
        Some(-32603),
        "a failed configure_initialize returns InternalError"
    );
    assert!(
        response(&outbox, 2).is_none(),
        "the conditional route never became observable after the transaction was discarded"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_conditional_registration_conflict_discards_the_transaction() {
    // A conditional registration that duplicates a static method fails combined
    // validation: InternalError, and neither contribution is exposed.
    let server = Server::builder(AppState::default())
        .request::<Ping, _, _>(ping)
        .configure_initialize(|_params, registrar| {
            // Duplicates the static `custom/ping` request — a DuplicateMethod
            // conflict surfaced when the transaction commits.
            registrar.request::<Ping, _, _>(ping);
            Ok(())
        })
        .build()
        .expect("server builds");

    let outbox = drive(server, vec![bare_initialize(1), notification("exit")]).await;

    assert_eq!(
        error_code(&outbox, 1),
        Some(-32603),
        "a combined-validation conflict returns InternalError"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_initialize_contributes_server_info_without_replacing_capabilities() {
    let server = Server::builder(AppState::default())
        .feature(lspf::features::hover(), hover)
        .on_initialize(|_state, _ctx, _params, _ct| async {
            Ok(Some(ServerInfo {
                name: "demo-server".to_string(),
                version: Some("1.2.3".to_string()),
            }))
        })
        .build()
        .expect("server builds");

    let outbox = drive(server, vec![bare_initialize(1), notification("exit")]).await;

    let init = initialize_result(&outbox, 1);
    assert_eq!(
        init.server_info,
        Some(ServerInfo {
            name: "demo-server".to_string(),
            version: Some("1.2.3".to_string()),
        }),
        "on_initialize contributes optional ServerInfo"
    );
    // The generated capabilities are unchanged by on_initialize: hover is still
    // advertised, plus protocol-owned position encoding, document sync, and
    // workspace-folder support.
    assert_eq!(
        init.capabilities,
        ServerCapabilities {
            hover_provider: Some(lspf::types::HoverProviderCapability::Simple(true)),
            position_encoding: Some(PositionEncodingKind::UTF16),
            text_document_sync: Some(lspf::types::TextDocumentSyncCapability::Kind(
                lspf::types::TextDocumentSyncKind::INCREMENTAL,
            )),
            workspace: Some(lspf::types::WorkspaceServerCapabilities {
                workspace_folders: Some(lspf::types::WorkspaceFoldersServerCapabilities {
                    supported: Some(true),
                    change_notifications: Some(lspf::types::OneOf::Left(true)),
                }),
                ..Default::default()
            }),
            ..ServerCapabilities::default()
        },
        "on_initialize cannot replace the framework-generated capabilities"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_initialize_observes_established_workspace_and_encoding() {
    // The Workspace, Documents encoding, and negotiated position encoding are
    // established from InitializeParams before on_initialize runs, so the hook
    // observes the final state.
    let observed_handle = Arc::new(Mutex::new(None));
    let server = Server::builder(AppState {
        observed: Arc::clone(&observed_handle),
    })
    .on_initialize(|state, ctx, _params, _ct| async move {
        let workspace = ctx.workspace();
        let observed = Observed {
            encoding: ctx.documents().position_encoding(),
            root_uri: workspace.root_uri().cloned(),
            folder_count: workspace.folders().len(),
        };
        *state.observed.lock().await = Some(observed);
        Ok(None)
    })
    .build()
    .expect("server builds");

    let outbox = drive(
        server,
        vec![
            initialize_request(
                1,
                json!({
                    "processId": null,
                    "rootUri": "file:///workspace/root",
                    "capabilities": {
                        "general": { "positionEncodings": ["utf-8"] }
                    },
                    "workspaceFolders": [
                        { "uri": "file:///workspace/root", "name": "root" }
                    ]
                }),
            ),
            notification("exit"),
        ],
    )
    .await;

    let init = initialize_result(&outbox, 1);
    assert_eq!(
        init.capabilities.position_encoding,
        Some(PositionEncodingKind::UTF8),
        "the client offered UTF-8, so it is negotiated and advertised"
    );

    let observed = observed_handle
        .lock()
        .await
        .clone()
        .expect("on_initialize ran and recorded what it observed");
    assert_eq!(
        observed.encoding,
        PositionEncoding::Utf8,
        "Documents encoding was established before on_initialize"
    );
    assert_eq!(
        observed.root_uri,
        Some("file:///workspace/root".parse::<Uri>().unwrap()),
        "the Workspace root was established from InitializeParams"
    );
    assert_eq!(
        observed.folder_count, 1,
        "the announced workspace folder was established before on_initialize"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_initialize_error_sends_that_error_and_closes() {
    let server = Server::builder(AppState::default())
        .on_initialize(|_state, _ctx, _params, _ct| async {
            Err(lspf::LspError::invalid_params("client is unsupported"))
        })
        .build()
        .expect("server builds");

    let outbox = drive(
        server,
        vec![
            bare_initialize(1),
            // Never answered: the failed on_initialize takes the close path.
            request(2, "shutdown", json!(null)),
            notification("exit"),
        ],
    )
    .await;

    assert_eq!(
        error_code(&outbox, 1),
        Some(-32602),
        "on_initialize failure sends that specific LspError, not the fixed InternalError"
    );
    assert!(
        response(&outbox, 2).is_none(),
        "the connection entered the close path instead of the running state"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_stores_the_complete_workspace_snapshot() {
    // The initialize transaction stores client info, capabilities,
    // initialization options, root URI, and folders — verbatim, folder order
    // included — so on_initialize (and every later handler) observes them all
    // through the established Workspace.
    let observed_handle = Arc::new(Mutex::new(None));
    let server = Server::builder(SnapshotState {
        observed: Arc::clone(&observed_handle),
    })
    .on_initialize(|state, ctx, _params, _ct| async move {
        let workspace = ctx.workspace();
        let snapshot = WorkspaceSnapshot {
            client_name: workspace.client_info().map(|info| info.name.clone()),
            client_version: workspace
                .client_info()
                .and_then(|info| info.version.clone()),
            initialization_options: workspace.initialization_options().cloned(),
            position_encodings: workspace
                .capabilities()
                .general
                .as_ref()
                .and_then(|general| general.position_encodings.clone()),
            root_uri: workspace.root_uri().cloned(),
            folder_names: workspace
                .folders()
                .iter()
                .map(|folder| folder.name.clone())
                .collect(),
            roots: roots_of(&ctx),
        };
        *state.observed.lock().await = Some(snapshot);
        Ok(None)
    })
    .build()
    .expect("server builds");

    let outbox = drive(
        server,
        vec![
            initialize_request(
                1,
                json!({
                    "processId": null,
                    "clientInfo": { "name": "vscode", "version": "1.100.0" },
                    "initializationOptions": { "settings": { "tabSize": 4 } },
                    "rootUri": "file:///workspace/root",
                    "capabilities": { "general": { "positionEncodings": ["utf-8"] } },
                    "workspaceFolders": [
                        { "uri": "file:///b", "name": "second" },
                        { "uri": "file:///a", "name": "first" }
                    ]
                }),
            ),
            notification("exit"),
        ],
    )
    .await;

    assert!(ok_result(&outbox, 1).is_some(), "initialize succeeds");
    let observed = observed_handle
        .lock()
        .await
        .clone()
        .expect("on_initialize ran and recorded the snapshot");
    assert_eq!(observed.client_name.as_deref(), Some("vscode"));
    assert_eq!(observed.client_version.as_deref(), Some("1.100.0"));
    assert_eq!(
        observed.initialization_options,
        Some(json!({ "settings": { "tabSize": 4 } })),
        "initialization options survive verbatim"
    );
    assert_eq!(
        observed.position_encodings,
        Some(vec![PositionEncodingKind::UTF8]),
        "client capabilities survive verbatim"
    );
    assert_eq!(
        observed.root_uri,
        Some("file:///workspace/root".parse::<Uri>().unwrap())
    );
    assert_eq!(
        observed.folder_names,
        ["second", "first"],
        "folder order is preserved"
    );
    assert_eq!(
        observed.roots,
        [
            ("file:///b".to_string(), "second".to_string()),
            ("file:///a".to_string(), "first".to_string()),
        ],
        "roots() prefers the announced folders over rootUri"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_without_folders_synthesizes_one_root_from_root_uri() {
    let observed_handle = Arc::new(Mutex::new(None));
    let server = Server::builder(SnapshotState {
        observed: Arc::clone(&observed_handle),
    })
    .on_initialize(|state, ctx, _params, _ct| async move {
        let snapshot = WorkspaceSnapshot {
            roots: roots_of(&ctx),
            ..WorkspaceSnapshot::default()
        };
        *state.observed.lock().await = Some(snapshot);
        Ok(None)
    })
    .build()
    .expect("server builds");

    let outbox = drive(
        server,
        vec![
            initialize_request(
                1,
                json!({
                    "processId": null,
                    "rootUri": "file:///workspace/root",
                    "capabilities": {}
                }),
            ),
            notification("exit"),
        ],
    )
    .await;

    assert!(ok_result(&outbox, 1).is_some(), "initialize succeeds");
    let observed = observed_handle
        .lock()
        .await
        .clone()
        .expect("on_initialize ran and recorded the roots");
    assert_eq!(
        observed.roots,
        [("file:///workspace/root".to_string(), "root".to_string())],
        "with no folders, roots() falls back to one synthetic root named for the final segment"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn documents_resolve_equivalent_uri_spellings_end_to_end() {
    // A document opened under one URI spelling is found through any equivalent
    // spelling — percent-encoded drive colon, drive-letter case — while the
    // public value keeps the original client URI.
    let server = Server::builder(AppState::default())
        .request::<DocProbe, _, _>(doc_probe)
        .build()
        .expect("server builds");

    let outbox = drive(
        server,
        vec![
            initialize_request(
                1,
                json!({
                    "processId": null,
                    "rootUri": "file:///C%3A/src",
                    "capabilities": {}
                }),
            ),
            notification_with(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": "file:///C%3A/src/main.rs",
                        "languageId": "rust",
                        "version": 1,
                        "text": "fn main() {}"
                    }
                }),
            ),
            request(
                2,
                "custom/docProbe",
                json!({ "uri": "file:///c:/src/main.rs" }),
            ),
            notification("exit"),
        ],
    )
    .await;

    let probe: DocProbeResult =
        serde_json::from_value(ok_result(&outbox, 2).expect("docProbe response")).unwrap();
    assert!(
        probe.found,
        "the probe's spelling resolves to the opened document"
    );
    assert_eq!(
        probe.opened_uri.as_deref(),
        Some("file:///C%3A/src/main.rs"),
        "the public value keeps the URI the client opened with"
    );
    assert_eq!(probe.text.as_deref(), Some("fn main() {}"));
}
