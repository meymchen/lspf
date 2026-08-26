//! ClientHandle-owned workspace queries leave the Workspace snapshot untouched
//! (issue #105).
//!
//! `ClientHandle::configuration` and `ClientHandle::workspace_folders` are read-only
//! queries: their results go to the caller and are never written into the
//! framework-owned Workspace state. The configuration snapshot stays under
//! `workspace/didChangeConfiguration` sync and the folder list stays under
//! `workspace/didChangeWorkspaceFolders` sync — a successful query result must
//! not disturb either, and later sync notifications must still apply.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use lspf::types::request::Request;
use lspf::types::{ConfigurationParams, WorkspaceFolder};
use lspf::{
    CancellationToken, ClientError, RawMessage, RequestId, Server, ServerContext, Transport,
    TransportError, TransportReader, TransportWriter,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

// --- Registrations -----------------------------------------------------------

/// A request whose handler runs the configuration query and records both the
/// helper result and the Workspace configuration snapshot it observed after
/// the query completed.
enum ConfigTrigger {}

impl Request for ConfigTrigger {
    type Params = ConfigurationParams;
    type Result = String;
    const METHOD: &'static str = "test/configTrigger";
}

/// A request whose handler runs the workspace-folders query and records both
/// the helper result and the Workspace folders it observed after the query
/// completed.
enum FoldersTrigger {}

impl Request for FoldersTrigger {
    type Params = ();
    type Result = String;
    const METHOD: &'static str = "test/foldersTrigger";
}

/// A probe returning the current Workspace configuration snapshot and folder
/// list, so the test can inspect framework state without sharing it.
#[derive(Debug, Deserialize, Serialize)]
struct WorkspaceProbeResult {
    configuration: Option<serde_json::Value>,
    folders: Vec<WorkspaceFolder>,
}

enum WorkspaceProbe {}

impl Request for WorkspaceProbe {
    type Params = ();
    type Result = WorkspaceProbeResult;
    const METHOD: &'static str = "test/workspaceProbe";
}

// --- Harness -----------------------------------------------------------------

struct ChannelTransport {
    incoming: mpsc::UnboundedReceiver<RawMessage>,
    outgoing: mpsc::UnboundedSender<RawMessage>,
}

struct ChannelReader(mpsc::UnboundedReceiver<RawMessage>);
struct ChannelWriter(mpsc::UnboundedSender<RawMessage>);

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (ChannelReader(self.incoming), ChannelWriter(self.outgoing))
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.0.recv().await.ok_or(TransportError::Closed)
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        self.0.send(message).map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

struct Session {
    in_tx: mpsc::UnboundedSender<RawMessage>,
    out_rx: mpsc::UnboundedReceiver<RawMessage>,
    serve: tokio::task::JoinHandle<lspf::Result<lspf::Outcome>>,
}

fn inbound_request(id: i32, method: &'static str, params: serde_json::Value) -> RawMessage {
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

fn success_response(id: i32, result: serde_json::Value) -> RawMessage {
    RawMessage::Response {
        id: RequestId::Number(id),
        result: Ok(Bytes::from(serde_json::to_vec(&result).unwrap())),
    }
}

fn exit() -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed("exit"),
        params: Bytes::from_static(b"null"),
    }
}

async fn receive(outgoing: &mut mpsc::UnboundedReceiver<RawMessage>) -> RawMessage {
    tokio::time::timeout(std::time::Duration::from_secs(2), outgoing.recv())
        .await
        .expect("server output before watchdog timeout")
        .expect("server output channel remains open")
}

async fn start(server: Server<()>) -> Session {
    let (in_tx, incoming) = mpsc::unbounded_channel();
    let (outgoing, out_rx) = mpsc::unbounded_channel();
    let serve = tokio::spawn(server.serve(ChannelTransport { incoming, outgoing }));
    Session {
        in_tx,
        out_rx,
        serve,
    }
}

async fn init_with_params(session: &mut Session, params: serde_json::Value) {
    session
        .in_tx
        .send(inbound_request(1, "initialize", params))
        .unwrap();
    let response = receive(&mut session.out_rx).await;
    assert_eq!(response.id(), Some(&RequestId::Number(1)));
}

async fn finish(session: Session) {
    session.in_tx.send(exit()).unwrap();
    session
        .serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
}

fn decode_result<T: DeserializeOwned>(response: &RawMessage) -> T {
    match response {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => serde_json::from_slice(bytes).expect("the result decodes"),
        other => panic!("expected a success response, got {other:?}"),
    }
}

/// The numeric ID of an outbound request, which is how the mock peer answers
/// it.
fn outbound_number_id(message: &RawMessage) -> i32 {
    match message {
        RawMessage::Request { id, .. } => match id {
            RequestId::Number(n) => *n,
            _ => panic!("expected a numeric outbound request id"),
        },
        other => panic!("expected a request, got {other:?}"),
    }
}

/// Send a workspace probe request and return the decoded snapshot.
async fn probe(session: &mut Session, id: i32) -> WorkspaceProbeResult {
    session
        .in_tx
        .send(inbound_request(id, WorkspaceProbe::METHOD, json!(null)))
        .unwrap();
    let response = receive(&mut session.out_rx).await;
    assert_eq!(response.id(), Some(&RequestId::Number(id)));
    decode_result(&response)
}

// --- Tests -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuration_result_never_enters_the_workspace_snapshot() {
    type Recorded = (
        Result<Vec<serde_json::Value>, ClientError>,
        Option<serde_json::Value>,
    );
    let recorded: Arc<Mutex<Option<Recorded>>> = Arc::new(Mutex::new(None));

    let recorded_handler = Arc::clone(&recorded);
    let server = Server::builder(())
        .request::<ConfigTrigger, _, _>(
            move |_state: Arc<()>,
                  ctx: ServerContext,
                  params: ConfigurationParams,
                  _ct: CancellationToken| {
                let recorded = Arc::clone(&recorded_handler);
                async move {
                    let result = ctx.client().configuration(params).await;
                    let snapshot = ctx.workspace().configuration();
                    *recorded.lock().unwrap() = Some((result, snapshot));
                    Ok("triggered".to_string())
                }
            },
        )
        .request::<WorkspaceProbe, _, _>(
            |_state: Arc<()>, ctx: ServerContext, _params: (), _ct: CancellationToken| async move {
                Ok(WorkspaceProbeResult {
                    configuration: ctx.workspace().configuration(),
                    folders: ctx.workspace().folders(),
                })
            },
        )
        .build()
        .expect("the workspace-query server builds");

    let mut session = start(server).await;
    init_with_params(
        &mut session,
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    )
    .await;

    // Establish a known snapshot through the protocol notification path.
    session
        .in_tx
        .send(notification(
            "workspace/didChangeConfiguration",
            json!({ "settings": { "tabSize": 2 } }),
        ))
        .unwrap();

    // Trigger the query and pin the outgoing request's wire shape.
    session
        .in_tx
        .send(inbound_request(
            2,
            ConfigTrigger::METHOD,
            json!({ "items": [
                { "section": "editor" },
                { "section": "lspf.language" },
                { "section": "lspf.trace" },
            ] }),
        ))
        .unwrap();
    let outbound = receive(&mut session.out_rx).await;
    let RawMessage::Request { method, params, .. } = &outbound else {
        panic!("expected an outgoing request, got {outbound:?}")
    };
    assert_eq!(method.as_ref(), "workspace/configuration");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(params).unwrap(),
        json!({ "items": [
            { "section": "editor" },
            { "section": "lspf.language" },
            { "section": "lspf.trace" },
        ] }),
        "the query items pass through verbatim"
    );

    // Answer with a result that differs from the snapshot and is one entry
    // shorter than the requested items: the framework must fill nothing in.
    let id = outbound_number_id(&outbound);
    session
        .in_tx
        .send(success_response(id, json!([{ "tabSize": 4 }, null])))
        .unwrap();

    // The trigger completes normally.
    let response = receive(&mut session.out_rx).await;
    assert_eq!(response.id(), Some(&RequestId::Number(2)));

    // The framework-owned snapshot is untouched by the query result.
    let snapshot = probe(&mut session, 3).await;
    assert_eq!(
        snapshot.configuration,
        Some(json!({ "tabSize": 2 })),
        "the query result never enters the configuration snapshot"
    );

    // The snapshot also stays live: a later notification still updates it.
    session
        .in_tx
        .send(notification(
            "workspace/didChangeConfiguration",
            json!({ "settings": { "tabSize": 8 } }),
        ))
        .unwrap();
    let snapshot = probe(&mut session, 4).await;
    assert_eq!(
        snapshot.configuration,
        Some(json!({ "tabSize": 8 })),
        "later didChangeConfiguration synchronization still applies"
    );

    finish(session).await;

    // The handler's recorded view agrees: the client's values came back in
    // order while the snapshot kept its pre-query value. The short reply
    // stays short — the framework does not fill in the missing entry.
    let (result, recorded_snapshot) = recorded.lock().unwrap().take().expect("recorded");
    assert_eq!(
        result.unwrap(),
        vec![json!({ "tabSize": 4 }), serde_json::Value::Null],
        "the client's order and length are preserved, with no filled-in values"
    );
    assert_eq!(recorded_snapshot, Some(json!({ "tabSize": 2 })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_folders_result_never_overwrites_the_folder_snapshot() {
    type Recorded = (
        Result<Option<Vec<WorkspaceFolder>>, ClientError>,
        Vec<WorkspaceFolder>,
    );
    let recorded: Arc<Mutex<Option<Recorded>>> = Arc::new(Mutex::new(None));

    let recorded_handler = Arc::clone(&recorded);
    let server = Server::builder(())
        .request::<FoldersTrigger, _, _>(
            move |_state: Arc<()>, ctx: ServerContext, _params: (), _ct: CancellationToken| {
                let recorded = Arc::clone(&recorded_handler);
                async move {
                    let result = ctx.client().workspace_folders().await;
                    let snapshot = ctx.workspace().folders();
                    *recorded.lock().unwrap() = Some((result, snapshot));
                    Ok("triggered".to_string())
                }
            },
        )
        .request::<WorkspaceProbe, _, _>(
            |_state: Arc<()>, ctx: ServerContext, _params: (), _ct: CancellationToken| async move {
                Ok(WorkspaceProbeResult {
                    configuration: ctx.workspace().configuration(),
                    folders: ctx.workspace().folders(),
                })
            },
        )
        .build()
        .expect("the workspace-query server builds");

    let mut session = start(server).await;
    init_with_params(
        &mut session,
        json!({
            "processId": null,
            "rootUri": null,
            "capabilities": {},
            "workspaceFolders": [{ "uri": "file:///orig", "name": "orig" }],
        }),
    )
    .await;

    // Trigger the query and pin the outgoing request's wire shape.
    session
        .in_tx
        .send(inbound_request(2, FoldersTrigger::METHOD, json!(null)))
        .unwrap();
    let outbound = receive(&mut session.out_rx).await;
    let RawMessage::Request { method, params, .. } = &outbound else {
        panic!("expected an outgoing request, got {outbound:?}")
    };
    assert_eq!(method.as_ref(), "workspace/workspaceFolders");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(params).unwrap(),
        serde_json::Value::Null,
        "the parameterless query sends null params"
    );

    // Answer with folders that differ from the initialized snapshot.
    let id = outbound_number_id(&outbound);
    session
        .in_tx
        .send(success_response(
            id,
            json!([{ "uri": "file:///other", "name": "other" }]),
        ))
        .unwrap();

    // The trigger completes normally.
    let response = receive(&mut session.out_rx).await;
    assert_eq!(response.id(), Some(&RequestId::Number(2)));

    // The framework-owned folder list is untouched by the query result.
    let snapshot = probe(&mut session, 3).await;
    assert_eq!(snapshot.folders.len(), 1);
    assert_eq!(snapshot.folders[0].uri.as_str(), "file:///orig");
    assert_eq!(
        snapshot.folders[0].name, "orig",
        "the query result never enters the folder list"
    );

    // The folder list also stays live: later synchronization still applies.
    session
        .in_tx
        .send(notification(
            "workspace/didChangeWorkspaceFolders",
            json!({ "event": {
                "added": [{ "uri": "file:///added", "name": "added" }],
                "removed": [],
            } }),
        ))
        .unwrap();
    let snapshot = probe(&mut session, 4).await;
    assert_eq!(snapshot.folders.len(), 2, "the added folder synchronized");
    assert_eq!(snapshot.folders[0].uri.as_str(), "file:///orig");
    assert_eq!(snapshot.folders[1].uri.as_str(), "file:///added");

    finish(session).await;

    // The handler's recorded view agrees: the client's folders came back while
    // the snapshot kept its pre-query list.
    let (result, recorded_snapshot) = recorded.lock().unwrap().take().expect("recorded");
    let queried = result.unwrap().expect("the client answered with folders");
    assert_eq!(queried.len(), 1);
    assert_eq!(queried[0].uri.as_str(), "file:///other");
    assert_eq!(queried[0].name, "other");
    assert_eq!(recorded_snapshot.len(), 1);
    assert_eq!(recorded_snapshot[0].uri.as_str(), "file:///orig");
}
