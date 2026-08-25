use std::borrow::Cow;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use lspf::types::request::Request;
use lspf::{
    Context, RawMessage, RequestId, Server, Transport, TransportError, TransportReader,
    TransportWriter,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing_subscriber::fmt::MakeWriter;

enum Echo {}

impl Request for Echo {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "test/echo";
}

enum PeerEcho {}

impl Request for PeerEcho {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "peer/echo";
}

enum CallsPeer {}

impl Request for CallsPeer {
    type Params = ();
    type Result = String;
    const METHOD: &'static str = "test/calls-peer";
}

enum Never {}

impl Request for Never {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "test/never";
}

enum CallsPeerTwice {}

impl Request for CallsPeerTwice {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "test/calls-peer-twice";
}

async fn echo(
    _state: Arc<()>,
    _ctx: Context,
    _params: String,
    _cancellation: lspf::CancellationToken,
) -> Result<String, lspf::LspError> {
    tracing::debug!("handler probe");
    Ok("secret-result".to_string())
}

async fn calls_peer(
    _state: Arc<()>,
    ctx: Context,
    (): (),
    _cancellation: lspf::CancellationToken,
) -> Result<String, lspf::LspError> {
    ctx.client()
        .request::<PeerEcho>("secret-outbound-params".to_string())
        .await
        .map_err(lspf::LspError::internal)
}

async fn never(
    _state: Arc<()>,
    _ctx: Context,
    (): (),
    cancellation: lspf::CancellationToken,
) -> Result<(), lspf::LspError> {
    cancellation.cancelled().await;
    std::future::pending().await
}

async fn calls_peer_twice(
    _state: Arc<()>,
    ctx: Context,
    (): (),
    _cancellation: lspf::CancellationToken,
) -> Result<(), lspf::LspError> {
    let client = ctx.client();
    let first = client.request::<PeerEcho>("first-secret".to_string());
    let second = client.request::<PeerEcho>("second-secret".to_string());
    let (first, second) = futures_util::future::join(first, second).await;
    first.map_err(lspf::LspError::internal)?;
    second.map_err(lspf::LspError::internal)?;
    Ok(())
}

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

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogBuffer {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl LogBuffer {
    fn events(&self) -> Vec<Value> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

fn capture_traces(logs: &LogBuffer) -> impl Drop {
    let subscriber = tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(logs.clone())
        .finish();
    tracing::subscriber::set_default(subscriber)
}

fn request(id: i32, method: &'static str, params: Value) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn notification(method: &'static str, params: Value) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

async fn receive_id(outgoing: &mut mpsc::UnboundedReceiver<RawMessage>, id: i32) {
    while outgoing.recv().await.unwrap().id() != Some(&RequestId::Number(id)) {}
}

async fn receive_method(
    outgoing: &mut mpsc::UnboundedReceiver<RawMessage>,
    method: &str,
) -> RequestId {
    loop {
        let message = outgoing.recv().await.unwrap();
        if message.method() == Some(method) {
            return message.id().unwrap().clone();
        }
    }
}

struct TestConnection {
    incoming: mpsc::UnboundedSender<RawMessage>,
    outgoing: mpsc::UnboundedReceiver<RawMessage>,
    serving: tokio::task::JoinHandle<lspf::Result<lspf::Outcome>>,
}

impl TestConnection {
    async fn start(server: Server<()>) -> Self {
        let (incoming, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, outgoing) = mpsc::unbounded_channel();
        let serving = tokio::spawn(server.serve(ChannelTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
        }));
        incoming
            .send(request(
                1,
                "initialize",
                json!({"processId": null, "rootUri": null, "capabilities": {}}),
            ))
            .unwrap();
        let mut connection = Self {
            incoming,
            outgoing,
            serving,
        };
        receive_id(&mut connection.outgoing, 1).await;
        connection
    }

    fn send(&self, message: RawMessage) {
        self.incoming.send(message).unwrap();
    }

    async fn stop(self) -> lspf::Outcome {
        self.send(notification("exit", Value::Null));
        self.serving.await.unwrap().unwrap()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn connection_messages_completion_and_close_use_the_stable_schema_without_payloads() {
    let logs = LogBuffer::default();
    let _guard = capture_traces(&logs);

    let server = Server::builder(())
        .request::<Echo, _, _>(echo)
        .build()
        .unwrap();
    let mut connection = TestConnection::start(server).await;
    connection.send(request(2, Echo::METHOD, json!("secret-params")));
    receive_id(&mut connection.outgoing, 2).await;
    connection.send(notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": "file:///secret-document.txt",
                "languageId": "text",
                "version": 1,
                "text": "secret-document-text"
            }
        }),
    ));
    assert_eq!(connection.stop().await.code(), 1);

    let events = logs.events();
    assert!(events.iter().any(|event| {
        event["message"] == "rpc message"
            && event["connection_id"].is_number()
            && event["direction"] == "inbound"
            && event["kind"] == "request"
            && event["method"] == "test/echo"
            && event["request_id"] == "2"
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "rpc message"
            && event["connection_id"].is_number()
            && event["direction"] == "outbound"
            && event["kind"] == "response"
            && event["request_id"] == "2"
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "request completed"
            && event["connection_id"].is_number()
            && event["direction"] == "inbound"
            && event["method"] == "test/echo"
            && event["request_id"] == "2"
            && event["latency_ms"].is_number()
            && event["completion"] == "success"
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "handler probe"
            && event["span"]["connection_id"].is_number()
            && event["span"]["direction"] == "inbound"
            && event["span"]["kind"] == "request"
            && event["span"]["method"] == "test/echo"
            && event["span"]["request_id"] == "2"
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "deadline changed"
            && event["deadline"] == "handler"
            && event["deadline_action"] == "armed"
            && event["method"] == "test/echo"
            && event["request_id"] == "2"
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "deadline changed"
            && event["deadline"] == "handler"
            && event["deadline_action"] == "completed"
            && event["method"] == "test/echo"
            && event["request_id"] == "2"
            && event["deadline_elapsed_ms"].is_number()
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "connection closed"
            && event["connection_id"].is_number()
            && event["close_cause"] == "exit"
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "resource budget changed"
            && event["resource"] == "inbound_requests"
            && event["resource_action"] == "admit"
            && event["resource_current"] == 1
            && event["resource_limit"] == 64
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "resource budget changed"
            && event["resource"] == "outbound_queue"
            && event["resource_action"] == "admit"
            && event["resource_current"] == 1
            && event["resource_limit"] == 1_024
            && event["resource_bytes"]
                .as_u64()
                .is_some_and(|bytes| bytes > 0)
            && event["resource_bytes_limit"] == 16 * 1024 * 1024
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "resource budget changed"
            && event["resource"] == "documents"
            && event["resource_action"] == "admit"
            && event["resource_current"] == 1
            && event["resource_limit"] == 1_024
            && event["resource_bytes"] == "secret-document-text".len()
            && event["resource_bytes_limit"] == 64 * 1024 * 1024
    }));

    let text = logs.text();
    for secret in ["secret-params", "secret-result", "secret-document-text"] {
        assert!(!text.contains(secret), "trace leaked {secret}: {text}");
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn pending_request_and_handler_deadline_events_expose_time_budget_usage() {
    let logs = LogBuffer::default();
    let _guard = capture_traces(&logs);

    let policy = lspf::ResourcePolicy {
        outbound_request_timeout: Some(Duration::from_secs(7)),
        handler_timeout: Duration::from_secs(5),
        ..lspf::ResourcePolicy::default()
    };
    let server = Server::builder(())
        .resource_policy(policy)
        .request::<CallsPeer, _, _>(calls_peer)
        .request::<Never, _, _>(never)
        .build()
        .unwrap();
    let mut connection = TestConnection::start(server).await;
    connection.send(request(2, CallsPeer::METHOD, Value::Null));
    let peer_id = receive_method(&mut connection.outgoing, PeerEcho::METHOD).await;
    connection.send(RawMessage::Response {
        id: peer_id,
        result: Ok(Bytes::from(
            serde_json::to_vec("secret-outbound-result").unwrap(),
        )),
    });
    receive_id(&mut connection.outgoing, 2).await;

    connection.send(request(3, Never::METHOD, Value::Null));
    tokio::time::advance(Duration::from_secs(5)).await;
    receive_id(&mut connection.outgoing, 3).await;
    connection.stop().await;

    let events = logs.events();
    assert!(events.iter().any(|event| {
        event["message"] == "resource budget changed"
            && event["resource"] == "pending_requests"
            && event["resource_action"] == "admit"
            && event["resource_current"] == 1
            && event["direction"] == "outbound"
            && event["method"] == PeerEcho::METHOD
            && event["deadline_ms"] == 7_000
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "resource budget changed"
            && event["resource"] == "pending_requests"
            && event["resource_action"] == "release"
            && event["resource_current"] == 0
            && event["direction"] == "outbound"
            && event["method"] == PeerEcho::METHOD
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "deadline changed"
            && event["deadline"] == "outbound_request"
            && event["deadline_action"] == "armed"
            && event["direction"] == "outbound"
            && event["method"] == PeerEcho::METHOD
            && event["deadline_ms"] == 7_000
            && event["deadline_elapsed_ms"] == 0
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "deadline changed"
            && event["deadline"] == "outbound_request"
            && event["deadline_action"] == "completed"
            && event["direction"] == "outbound"
            && event["method"] == PeerEcho::METHOD
            && event["deadline_ms"] == 7_000
            && event["deadline_elapsed_ms"].is_number()
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "request completed"
            && event["direction"] == "outbound"
            && event["method"] == PeerEcho::METHOD
            && event["latency_ms"].is_number()
            && event["completion"] == "success"
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "deadline changed"
            && event["deadline"] == "handler"
            && event["deadline_action"] == "armed"
            && event["direction"] == "inbound"
            && event["method"] == Never::METHOD
            && event["request_id"] == "3"
            && event["deadline_ms"] == 5_000
            && event["deadline_elapsed_ms"] == 0
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "deadline changed"
            && event["deadline"] == "handler"
            && event["deadline_action"] == "expired"
            && event["direction"] == "inbound"
            && event["method"] == Never::METHOD
            && event["request_id"] == "3"
            && event["deadline_ms"] == 5_000
            && event["deadline_elapsed_ms"] == 5_000
    }));

    let text = logs.text();
    assert!(!text.contains("secret-outbound-params"));
    assert!(!text.contains("secret-outbound-result"));
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_pending_releases_report_each_atomic_depth_transition() {
    let logs = LogBuffer::default();
    let _guard = capture_traces(&logs);
    let server = Server::builder(())
        .request::<CallsPeerTwice, _, _>(calls_peer_twice)
        .build()
        .unwrap();
    let mut connection = TestConnection::start(server).await;
    connection.send(request(2, CallsPeerTwice::METHOD, Value::Null));
    let first_id = receive_method(&mut connection.outgoing, PeerEcho::METHOD).await;
    let second_id = receive_method(&mut connection.outgoing, PeerEcho::METHOD).await;
    for id in [first_id, second_id] {
        connection.send(RawMessage::Response {
            id,
            result: Ok(Bytes::from(serde_json::to_vec("ok").unwrap())),
        });
    }
    receive_id(&mut connection.outgoing, 2).await;
    connection.stop().await;

    let release_depths: Vec<_> = logs
        .events()
        .into_iter()
        .filter(|event| {
            event["message"] == "resource budget changed"
                && event["resource"] == "pending_requests"
                && event["resource_action"] == "release"
                && event["method"] == PeerEcho::METHOD
        })
        .map(|event| event["resource_current"].as_u64().unwrap())
        .collect();
    assert_eq!(release_depths, [1, 0]);
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_document_change_reports_the_unchanged_byte_budget() {
    let logs = LogBuffer::default();
    let _guard = capture_traces(&logs);
    let policy = lspf::ResourcePolicy {
        max_document_bytes: 4,
        ..lspf::ResourcePolicy::default()
    };
    let server = Server::builder(()).resource_policy(policy).build().unwrap();
    let connection = TestConnection::start(server).await;
    connection.send(notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": "file:///budget.txt",
                "languageId": "text",
                "version": 1,
                "text": "okay"
            }
        }),
    ));
    connection.send(notification(
        "textDocument/didChange",
        json!({
            "textDocument": {"uri": "file:///budget.txt", "version": 2},
            "contentChanges": [{"text": "too-long"}]
        }),
    ));
    connection.stop().await;

    assert!(logs.events().iter().any(|event| {
        event["message"] == "resource budget changed"
            && event["resource"] == "documents"
            && event["resource_action"] == "reject"
            && event["resource_current"] == 1
            && event["resource_bytes"] == 4
            && event["resource_bytes_limit"] == 4
    }));
}
