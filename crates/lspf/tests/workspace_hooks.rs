//! End-to-end coverage for post-mutation workspace hooks (issue #71).

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use lspf::types::notification::{DidChangeConfiguration, DidChangeWorkspaceFolders, SetTrace};
use lspf::types::request::Request;
use lspf::types::{
    DidChangeConfigurationParams, DidChangeWorkspaceFoldersParams, SetTraceParams, TraceValue,
    WorkspaceFoldersServerCapabilities,
};
use lspf::{
    CancellationToken, Context, LspError, RawMessage, RequestId, Server, Transport, TransportError,
    TransportReader, TransportWriter,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

type ObservedFolderSnapshots = Arc<Mutex<Vec<Vec<(String, String)>>>>;

struct AppState {
    folder_snapshots: ObservedFolderSnapshots,
    configurations: Arc<Mutex<Vec<Option<serde_json::Value>>>>,
    traces: Arc<Mutex<Vec<TraceValue>>>,
}

async fn on_configuration(
    state: Arc<AppState>,
    ctx: Context,
    _params: DidChangeConfigurationParams,
) {
    state
        .configurations
        .lock()
        .unwrap()
        .push(ctx.workspace().configuration());
}

async fn on_trace(state: Arc<AppState>, ctx: Context, _params: SetTraceParams) {
    state.traces.lock().unwrap().push(ctx.workspace().trace());
}

fn folders(ctx: &Context) -> Vec<(String, String)> {
    ctx.workspace()
        .folders()
        .iter()
        .map(|folder| (folder.uri.as_str().to_string(), folder.name.clone()))
        .collect()
}

async fn on_folders(state: Arc<AppState>, ctx: Context, _params: DidChangeWorkspaceFoldersParams) {
    state.folder_snapshots.lock().unwrap().push(folders(&ctx));
}

#[derive(Deserialize, Serialize)]
struct ProbeParams {}

#[derive(Debug, Deserialize, Serialize)]
struct ProbeResult {
    folders: Vec<(String, String)>,
    configuration: Option<serde_json::Value>,
    trace: TraceValue,
}

enum Probe {}

impl Request for Probe {
    type Params = ProbeParams;
    type Result = ProbeResult;
    const METHOD: &'static str = "custom/workspaceProbe";
}

async fn probe(
    _state: Arc<AppState>,
    ctx: Context,
    _params: ProbeParams,
    _ct: CancellationToken,
) -> Result<ProbeResult, LspError> {
    Ok(ProbeResult {
        folders: folders(&ctx),
        configuration: ctx.workspace().configuration(),
        trace: ctx.workspace().trace(),
    })
}

struct ChannelTransport {
    in_rx: mpsc::UnboundedReceiver<RawMessage>,
    out_tx: mpsc::UnboundedSender<RawMessage>,
}

struct ChannelReader(mpsc::UnboundedReceiver<RawMessage>);
struct ChannelWriter(mpsc::UnboundedSender<RawMessage>);

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (ChannelReader(self.in_rx), ChannelWriter(self.out_tx))
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.0.recv().await.ok_or(TransportError::Closed)
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, msg: RawMessage) -> Result<(), TransportError> {
        self.0.send(msg).map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

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

async fn drive(server: Server<AppState>, messages: Vec<RawMessage>) -> Vec<RawMessage> {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let handle = tokio::spawn(async move {
        server
            .serve(ChannelTransport { in_rx, out_tx })
            .await
            .expect("serve ended cleanly")
    });

    let mut outbox = Vec::new();
    for msg in messages {
        let response_id = msg.id().cloned();
        in_tx.send(msg).unwrap();
        if let Some(response_id) = response_id {
            let response = tokio::time::timeout(Duration::from_secs(2), out_rx.recv())
                .await
                .expect("response arrived")
                .expect("writer stayed open");
            assert_eq!(response.id(), Some(&response_id));
            outbox.push(response);
        }
    }
    drop(in_tx);
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("serve returned")
        .expect("server did not panic");
    outbox.extend(std::iter::from_fn(|| out_rx.try_recv().ok()));
    outbox
}

fn result(outbox: &[RawMessage], id: i32) -> serde_json::Value {
    match outbox
        .iter()
        .find(|message| message.id() == Some(&RequestId::Number(id)))
        .expect("request was answered")
    {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => serde_json::from_slice(bytes).unwrap(),
        other => panic!("expected success, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_hook_and_later_handler_observe_normalized_ordered_mutation() {
    let folder_snapshots: ObservedFolderSnapshots = Arc::default();
    let server = Server::builder(AppState {
        folder_snapshots: Arc::clone(&folder_snapshots),
        configurations: Arc::default(),
        traces: Arc::default(),
    })
    .notification::<DidChangeWorkspaceFolders, _, _>(on_folders)
    .request::<Probe, _, _>(probe)
    .build()
    .expect("server builds");

    let outbox = drive(
        server,
        vec![
            request(
                1,
                "initialize",
                json!({
                    "processId": null,
                    "rootUri": null,
                    "capabilities": {},
                    "workspaceFolders": [
                        { "uri": "file:///C%3A/first", "name": "first" },
                        { "uri": "file:///second", "name": "second" }
                    ]
                }),
            ),
            notification(
                "workspace/didChangeWorkspaceFolders",
                json!({ "event": {
                    "removed": [
                        { "uri": "file:///c:/first", "name": "ignored spelling" },
                        { "uri": "file:///unknown", "name": "unknown" }
                    ],
                    "added": [
                        { "uri": "FILE:///second", "name": "renamed" },
                        { "uri": "file:///third", "name": "third" }
                    ]
                }}),
            ),
            request(2, "custom/workspaceProbe", json!({})),
            notification("exit", json!(null)),
        ],
    )
    .await;

    let expected = vec![
        ("file:///second".to_string(), "renamed".to_string()),
        ("file:///third".to_string(), "third".to_string()),
    ];
    assert_eq!(*folder_snapshots.lock().unwrap(), vec![expected.clone()]);
    let probe: ProbeResult = serde_json::from_value(result(&outbox, 2)).unwrap();
    assert_eq!(probe.folders, expected);

    let initialize = result(&outbox, 1);
    let workspace_folders: WorkspaceFoldersServerCapabilities =
        serde_json::from_value(initialize["capabilities"]["workspace"]["workspaceFolders"].clone())
            .unwrap();
    let workspace_folders = serde_json::to_string(&workspace_folders).unwrap();
    assert_eq!(
        workspace_folders,
        include_str!("fixtures/workspace_folders_capability.json").trim_end(),
        "multi-root initialization has a stable framework-owned capability"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuration_hook_and_later_handler_observe_latest_raw_value() {
    let configurations = Arc::default();
    let server = Server::builder(AppState {
        folder_snapshots: Arc::default(),
        configurations: Arc::clone(&configurations),
        traces: Arc::default(),
    })
    .notification::<DidChangeConfiguration, _, _>(on_configuration)
    .request::<Probe, _, _>(probe)
    .build()
    .expect("server builds");

    let outbox = drive(
        server,
        vec![
            request(
                1,
                "initialize",
                json!({ "processId": null, "rootUri": null, "capabilities": {} }),
            ),
            notification(
                "workspace/didChangeConfiguration",
                json!({ "settings": { "nested": [1, null, { "enabled": true }] } }),
            ),
            notification(
                "workspace/didChangeConfiguration",
                json!({ "settings": "replacement" }),
            ),
            request(2, "custom/workspaceProbe", json!({})),
            notification("exit", json!(null)),
        ],
    )
    .await;

    assert_eq!(
        *configurations.lock().unwrap(),
        vec![
            Some(json!({ "nested": [1, null, { "enabled": true }] })),
            Some(json!("replacement")),
        ]
    );
    let probe: ProbeResult = serde_json::from_value(result(&outbox, 2)).unwrap();
    assert_eq!(probe.configuration, Some(json!("replacement")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trace_starts_off_and_malformed_update_preserves_state_and_skips_hook() {
    let traces = Arc::default();
    let server = Server::builder(AppState {
        folder_snapshots: Arc::default(),
        configurations: Arc::default(),
        traces: Arc::clone(&traces),
    })
    .notification::<SetTrace, _, _>(on_trace)
    .request::<Probe, _, _>(probe)
    .build()
    .expect("server builds");

    let outbox = drive(
        server,
        vec![
            request(
                1,
                "initialize",
                json!({ "processId": null, "rootUri": null, "capabilities": {} }),
            ),
            request(2, "custom/workspaceProbe", json!({})),
            notification("$/setTrace", json!({ "value": "verbose" })),
            notification("$/setTrace", json!({ "value": "invalid" })),
            request(3, "custom/workspaceProbe", json!({})),
            notification("exit", json!(null)),
        ],
    )
    .await;

    let before: ProbeResult = serde_json::from_value(result(&outbox, 2)).unwrap();
    assert_eq!(before.trace, TraceValue::Off);
    assert_eq!(*traces.lock().unwrap(), vec![TraceValue::Verbose]);
    let after: ProbeResult = serde_json::from_value(result(&outbox, 3)).unwrap();
    assert_eq!(after.trace, TraceValue::Verbose);
}
