use std::borrow::Cow;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use lsp_types::{LogMessageParams, MessageType};
use lspf::types::request::Request;
use lspf::{
    Client, ClientError, Context, DocumentsView, RawMessage, RequestId, ResourcePolicy, Server,
    Transport, TransportError, TransportReader, TransportWriter,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};
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

enum OverloadsPeer {}

impl Request for OverloadsPeer {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "test/overloads-peer";
}

enum Stalls {}

impl Request for Stalls {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "test/stalls";
}

enum FillsQueue {}

impl Request for FillsQueue {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "test/fills-queue";
}

enum CaptureHandles {}

impl Request for CaptureHandles {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "test/capture-handles";
}

#[derive(Default)]
struct HandlerTaskUsage {
    current: AtomicUsize,
    peak: AtomicUsize,
}

impl HandlerTaskUsage {
    fn enter(self: &Arc<Self>) -> HandlerTaskGuard {
        let current = self.current.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak.fetch_max(current, Ordering::AcqRel);
        HandlerTaskGuard(Arc::clone(self))
    }
}

struct HandlerTaskGuard(Arc<HandlerTaskUsage>);

impl Drop for HandlerTaskGuard {
    fn drop(&mut self) {
        self.0.current.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Default)]
struct StressState {
    handler_tasks: Arc<HandlerTaskUsage>,
    queue_rejected: Arc<Notify>,
    resume_queue_handler: Arc<Notify>,
}

#[derive(Clone, Default)]
struct CleanupState {
    handler_tasks: Arc<HandlerTaskUsage>,
    client: Arc<Mutex<Option<Client>>>,
    documents: Arc<Mutex<Option<DocumentsView>>>,
}

trait HasHandlerTaskUsage {
    fn handler_tasks(&self) -> &Arc<HandlerTaskUsage>;
}

impl HasHandlerTaskUsage for StressState {
    fn handler_tasks(&self) -> &Arc<HandlerTaskUsage> {
        &self.handler_tasks
    }
}

impl HasHandlerTaskUsage for CleanupState {
    fn handler_tasks(&self) -> &Arc<HandlerTaskUsage> {
        &self.handler_tasks
    }
}

async fn stalls<S>(
    state: Arc<S>,
    _ctx: Context,
    (): (),
    cancellation: lspf::CancellationToken,
) -> Result<(), lspf::LspError>
where
    S: HasHandlerTaskUsage,
{
    let _task = state.handler_tasks().enter();
    cancellation.cancelled().await;
    std::future::pending().await
}

async fn fills_queue(
    state: Arc<StressState>,
    ctx: Context,
    (): (),
    _cancellation: lspf::CancellationToken,
) -> Result<(), lspf::LspError> {
    let _task = state.handler_tasks.enter();
    let message = "x".repeat(12 * 1024);
    ctx.client()
        .log_message(LogMessageParams {
            typ: MessageType::INFO,
            message: message.clone(),
        })
        .map_err(lspf::LspError::internal)?;
    assert!(matches!(
        ctx.client().log_message(LogMessageParams {
            typ: MessageType::INFO,
            message,
        }),
        Err(ClientError::OutboundOverloaded)
    ));
    state.queue_rejected.notify_one();
    state.resume_queue_handler.notified().await;
    Ok(())
}

async fn capture_handles(
    state: Arc<CleanupState>,
    ctx: Context,
    (): (),
    _cancellation: lspf::CancellationToken,
) -> Result<(), lspf::LspError> {
    *state.client.lock().unwrap() = Some(ctx.client());
    *state.documents.lock().unwrap() = Some(ctx.documents());
    Ok(())
}

#[derive(Clone, Default)]
struct OverloadState {
    rejected: Arc<Notify>,
    resume_handler: Arc<Notify>,
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

async fn overloads_peer(
    state: Arc<OverloadState>,
    ctx: Context,
    (): (),
    _cancellation: lspf::CancellationToken,
) -> Result<(), lspf::LspError> {
    ctx.client()
        .log_message(LogMessageParams {
            typ: MessageType::INFO,
            message: "secret-queued-notification".to_string(),
        })
        .map_err(lspf::LspError::internal)?;
    match ctx
        .client()
        .request::<PeerEcho>("secret-rejected-request".to_string())
        .await
    {
        Err(ClientError::OutboundOverloaded) => {}
        outcome => {
            return Err(lspf::LspError::internal(io::Error::other(format!(
                "expected outbound overload, got {outcome:?}"
            ))));
        }
    }
    state.rejected.notify_one();
    state.resume_handler.notified().await;
    Ok(())
}

#[derive(Default)]
/// Controls outbound transport progress. Pausing `send` models a peer that is
/// not reading server output, while failing it models a terminal writer error.
struct OutboundTransportControl {
    pause_next: AtomicBool,
    fail_next: AtomicBool,
    entered: Notify,
    resume: Notify,
}

impl OutboundTransportControl {
    fn pause_next(&self) {
        self.pause_next.store(true, Ordering::Release);
    }

    async fn wait_until_paused(&self) {
        self.entered.notified().await;
    }

    fn resume(&self) {
        self.resume.notify_one();
    }

    fn fail_next(&self) {
        self.fail_next.store(true, Ordering::Release);
    }
}

struct ChannelTransport {
    incoming: mpsc::UnboundedReceiver<RawMessage>,
    outgoing: mpsc::UnboundedSender<RawMessage>,
    outbound_control: Arc<OutboundTransportControl>,
}

struct ChannelReader(mpsc::UnboundedReceiver<RawMessage>);
struct ChannelWriter(
    mpsc::UnboundedSender<RawMessage>,
    Arc<OutboundTransportControl>,
);

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            ChannelReader(self.incoming),
            ChannelWriter(self.outgoing, self.outbound_control),
        )
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.0.recv().await.ok_or(TransportError::Closed)
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        if self.1.pause_next.swap(false, Ordering::AcqRel) {
            self.1.entered.notify_one();
            self.1.resume.notified().await;
        }
        if self.1.fail_next.swap(false, Ordering::AcqRel) {
            return Err(TransportError::Closed);
        }
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

async fn receive_notification_method(
    outgoing: &mut mpsc::UnboundedReceiver<RawMessage>,
    method: &str,
) {
    while outgoing.recv().await.unwrap().method() != Some(method) {}
}

async fn wait_for_event(logs: &LogBuffer, predicate: impl Fn(&Value) -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if logs.events().iter().any(&predicate) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the expected trace event is emitted");
}

async fn wait_for_handler_task_count(tasks: &HandlerTaskUsage, expected: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if tasks.current.load(Ordering::Acquire) == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("handler task count reached the expected value");
}

async fn wait_for_notification(notify: &Notify, message: &str) {
    tokio::time::timeout(Duration::from_secs(1), notify.notified())
        .await
        .unwrap_or_else(|_| panic!("{message}"));
}

fn assert_resource_events_stay_within_limits(events: &[Value]) {
    for event in events
        .iter()
        .filter(|event| event["message"] == "resource budget changed")
    {
        if let (Some(current), Some(limit)) = (
            event["resource_current"].as_u64(),
            event["resource_limit"].as_u64(),
        ) {
            assert!(
                current <= limit,
                "resource count exceeded its limit: {event}"
            );
        }
        if let (Some(bytes), Some(limit)) = (
            event["resource_bytes"].as_u64(),
            event["resource_bytes_limit"].as_u64(),
        ) {
            assert!(
                bytes <= limit,
                "resource bytes exceeded their limit: {event}"
            );
        }
    }
}

fn assert_resource_finished_at_zero(events: &[Value], resource: &str) {
    let last = events
        .iter()
        .rev()
        .find(|event| {
            event["message"] == "resource budget changed" && event["resource"] == resource
        })
        .unwrap_or_else(|| panic!("no resource event for {resource}"));
    assert_eq!(
        last["resource_current"], 0,
        "{resource} did not finish empty: {last}"
    );
    if last["resource_bytes"].is_number() {
        assert_eq!(
            last["resource_bytes"], 0,
            "{resource} bytes did not finish empty: {last}"
        );
    }
}

fn completion_count(events: &[Value], direction: &str, method: &str, request_id: &str) -> usize {
    events
        .iter()
        .filter(|event| {
            event["message"] == "request completed"
                && event["direction"] == direction
                && event["method"] == method
                && event["request_id"] == request_id
        })
        .count()
}

fn request_id_text(id: &RequestId) -> String {
    match id {
        RequestId::Number(id) => id.to_string(),
        RequestId::String(id) => id.clone(),
    }
}

async fn await_outcome(
    serving: tokio::task::JoinHandle<lspf::Result<lspf::Outcome>>,
    close_description: &str,
) -> lspf::Outcome {
    tokio::time::timeout(Duration::from_secs(2), serving)
        .await
        .unwrap_or_else(|_| panic!("{close_description} did not close the connection"))
        .expect("serve task did not panic")
        .unwrap_or_else(|error| panic!("{close_description} returned an error: {error}"))
}

struct TestConnection {
    incoming: mpsc::UnboundedSender<RawMessage>,
    outgoing: mpsc::UnboundedReceiver<RawMessage>,
    serving: tokio::task::JoinHandle<lspf::Result<lspf::Outcome>>,
    outbound_control: Arc<OutboundTransportControl>,
}

impl TestConnection {
    async fn start<S>(server: Server<S>) -> Self
    where
        S: Send + Sync + 'static,
    {
        let (incoming, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, outgoing) = mpsc::unbounded_channel();
        let outbound_control = Arc::new(OutboundTransportControl::default());
        let serving = tokio::spawn(server.serve(ChannelTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            outbound_control: Arc::clone(&outbound_control),
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
            outbound_control,
        };
        receive_id(&mut connection.outgoing, 1).await;
        connection
    }

    fn send(&self, message: RawMessage) {
        self.incoming.send(message).unwrap();
    }

    fn pause_next_write(&self) {
        self.outbound_control.pause_next();
    }

    fn fail_next_write(&self) {
        self.outbound_control.fail_next();
    }

    async fn stop(self) -> lspf::Outcome {
        self.send(notification("exit", Value::Null));
        self.serving.await.unwrap().unwrap()
    }

    async fn close_from_eof(self) -> lspf::Outcome {
        let Self {
            incoming,
            outgoing: _,
            serving,
            outbound_control: _,
        } = self;
        drop(incoming);
        await_outcome(serving, "EOF").await
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
    let peer_id_text = request_id_text(&peer_id);
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
    assert_eq!(
        completion_count(&events, "outbound", PeerEcho::METHOD, &peer_id_text),
        1
    );
    assert!(events.iter().any(|event| {
        event["message"] == "request completed"
            && event["direction"] == "outbound"
            && event["method"] == PeerEcho::METHOD
            && event["request_id"] == peer_id_text
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
async fn outbound_queue_rejection_completes_the_rolled_back_request_as_rejected() {
    let logs = LogBuffer::default();
    let _guard = capture_traces(&logs);
    let state = OverloadState::default();
    let policy = lspf::ResourcePolicy {
        max_outbound_messages: 1,
        ..lspf::ResourcePolicy::default()
    };
    let server = Server::builder(state.clone())
        .resource_policy(policy)
        .request::<OverloadsPeer, _, _>(overloads_peer)
        .build()
        .unwrap();
    let mut connection = TestConnection::start(server).await;

    connection.pause_next_write();
    connection.send(request(2, OverloadsPeer::METHOD, Value::Null));
    connection.outbound_control.wait_until_paused().await;
    state.rejected.notified().await;

    let events = logs.events();
    assert!(events.iter().any(|event| {
        event["message"] == "resource budget changed"
            && event["resource"] == "pending_requests"
            && event["resource_action"] == "rollback"
            && event["resource_current"] == 0
            && event["direction"] == "outbound"
            && event["method"] == PeerEcho::METHOD
    }));
    assert!(events.iter().any(|event| {
        event["message"] == "request completed"
            && event["direction"] == "outbound"
            && event["method"] == PeerEcho::METHOD
            && event["latency_ms"].is_number()
            && event["completion"] == "rejected"
    }));

    connection.outbound_control.resume();
    receive_notification_method(&mut connection.outgoing, "window/logMessage").await;
    wait_for_event(&logs, |event| {
        event["message"] == "resource budget changed"
            && event["resource"] == "outbound_queue"
            && event["resource_action"] == "release"
            && event["resource_current"] == 0
    })
    .await;
    state.resume_handler.notify_one();
    receive_id(&mut connection.outgoing, 2).await;
    connection.stop().await;

    let text = logs.text();
    assert!(!text.contains("secret-queued-notification"));
    assert!(!text.contains("secret-rejected-request"));
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

#[tokio::test(flavor = "current_thread")]
async fn fixed_budget_floods_and_a_slow_reader_never_exceed_connection_limits() {
    const OUTBOUND_BYTES: usize = 16 * 1024;

    let logs = LogBuffer::default();
    let _guard = capture_traces(&logs);
    let state = StressState::default();
    let policy = ResourcePolicy {
        max_inbound_requests: 2,
        max_outbound_messages: 8,
        max_outbound_bytes: OUTBOUND_BYTES,
        max_documents: 2,
        max_document_bytes: 8,
        handler_timeout: Duration::from_secs(120),
        ..ResourcePolicy::default()
    };
    let server = Server::builder(state.clone())
        .resource_policy(policy)
        .request::<Stalls, _, _>(stalls::<StressState>)
        .request::<FillsQueue, _, _>(fills_queue)
        .build()
        .unwrap();
    let mut connection = TestConnection::start(server).await;

    for (name, text) in [("first", "1234"), ("second", "5678"), ("excess", "x")] {
        connection.send(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": format!("file:///{name}.txt"),
                    "languageId": "text",
                    "version": 1,
                    "text": text
                }
            }),
        ));
    }
    wait_for_event(&logs, |event| {
        event["resource"] == "documents" && event["resource_action"] == "reject"
    })
    .await;

    connection.send(request(2, Stalls::METHOD, Value::Null));
    connection.send(request(3, Stalls::METHOD, Value::Null));
    wait_for_handler_task_count(&state.handler_tasks, 2).await;
    connection.send(request(4, Stalls::METHOD, Value::Null));
    receive_id(&mut connection.outgoing, 4).await;

    connection.send(notification("$/cancelRequest", json!({"id": 2})));
    receive_id(&mut connection.outgoing, 2).await;
    wait_for_handler_task_count(&state.handler_tasks, 1).await;

    connection.pause_next_write();
    connection.send(request(5, FillsQueue::METHOD, Value::Null));
    tokio::time::timeout(
        Duration::from_secs(1),
        connection.outbound_control.wait_until_paused(),
    )
    .await
    .expect("the slow writer reached its deterministic pause");
    wait_for_notification(
        &state.queue_rejected,
        "the byte-limited queue rejected the second message",
    )
    .await;

    let events_during_load = logs.events();
    assert_resource_events_stay_within_limits(&events_during_load);
    assert_eq!(state.handler_tasks.peak.load(Ordering::Acquire), 2);
    assert!(events_during_load.iter().any(|event| {
        event["resource"] == "inbound_requests"
            && event["resource_action"] == "reject"
            && event["resource_current"] == 2
            && event["resource_limit"] == 2
    }));
    assert!(events_during_load.iter().any(|event| {
        event["resource"] == "outbound_queue"
            && event["resource_action"] == "reject"
            && event["resource_bytes"]
                .as_u64()
                .is_some_and(|bytes| bytes > 0)
            && event["resource_bytes_limit"] == OUTBOUND_BYTES
    }));
    assert!(events_during_load.iter().any(|event| {
        event["resource"] == "documents"
            && event["resource_action"] == "reject"
            && event["resource_current"] == 2
            && event["resource_bytes"] == 8
    }));

    connection.outbound_control.resume();
    wait_for_event(&logs, |event| {
        event["resource"] == "outbound_queue"
            && event["resource_action"] == "release"
            && event["resource_current"] == 0
    })
    .await;
    state.resume_queue_handler.notify_one();
    receive_id(&mut connection.outgoing, 5).await;

    assert_eq!(
        connection.close_from_eof().await,
        lspf::Outcome::TransportClosed
    );
    wait_for_handler_task_count(&state.handler_tasks, 0).await;

    let events = logs.events();
    assert_resource_events_stay_within_limits(&events);
    for resource in ["inbound_requests", "outbound_queue", "documents"] {
        assert_resource_finished_at_zero(&events, resource);
    }
    assert_eq!(completion_count(&events, "inbound", Stalls::METHOD, "2"), 1);
    assert_eq!(completion_count(&events, "inbound", Stalls::METHOD, "3"), 1);
    assert_eq!(
        completion_count(&events, "inbound", FillsQueue::METHOD, "5"),
        1
    );
}

#[derive(Clone, Copy)]
enum CleanupTrigger {
    Eof,
    WriterFailure,
    ShutdownThenExit,
}

async fn assert_close_clears_every_connection_resource(trigger: CleanupTrigger) {
    let logs = LogBuffer::default();
    let _guard = capture_traces(&logs);
    let state = CleanupState::default();
    let server = Server::builder(state.clone())
        .resource_policy(ResourcePolicy {
            outbound_request_timeout: Some(Duration::from_secs(120)),
            handler_timeout: Duration::from_secs(120),
            ..ResourcePolicy::default()
        })
        .request::<CaptureHandles, _, _>(capture_handles)
        .request::<Stalls, _, _>(stalls::<CleanupState>)
        .build()
        .unwrap();
    let mut connection = TestConnection::start(server).await;

    connection.send(notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": "file:///retained.txt",
                "languageId": "text",
                "version": 1,
                "text": "released on close"
            }
        }),
    ));
    connection.send(request(2, CaptureHandles::METHOD, Value::Null));
    receive_id(&mut connection.outgoing, 2).await;
    let client = state
        .client
        .lock()
        .unwrap()
        .clone()
        .expect("client captured");
    let documents = state
        .documents
        .lock()
        .unwrap()
        .clone()
        .expect("documents captured");

    let progress_client = client.clone();
    let progress = tokio::spawn(async move {
        progress_client
            .begin_progress(lspf::ProgressOptions::new("cleanup probe"))
            .await
    });
    let create_id =
        receive_method(&mut connection.outgoing, "window/workDoneProgress/create").await;
    connection.send(RawMessage::Response {
        id: create_id,
        result: Ok(Bytes::from_static(b"null")),
    });
    receive_notification_method(&mut connection.outgoing, "$/progress").await;
    let progress = progress
        .await
        .expect("progress task did not panic")
        .expect("progress began");

    let pending_client = client.clone();
    let pending = tokio::spawn(async move {
        pending_client
            .request::<PeerEcho>("never answered".to_string())
            .await
    });
    let peer_id = receive_method(&mut connection.outgoing, PeerEcho::METHOD).await;
    let peer_id_text = request_id_text(&peer_id);

    connection.send(request(3, Stalls::METHOD, Value::Null));
    wait_for_handler_task_count(&state.handler_tasks, 1).await;
    connection.pause_next_write();
    client
        .log_message(LogMessageParams {
            typ: MessageType::INFO,
            message: "accounted until close".to_string(),
        })
        .expect("the final write is admitted");
    tokio::time::timeout(
        Duration::from_secs(1),
        connection.outbound_control.wait_until_paused(),
    )
    .await
    .expect("the final accounted write reached its deterministic pause");

    let (outcome, expected_outcome) = match trigger {
        CleanupTrigger::Eof => {
            connection.outbound_control.resume();
            (
                connection.close_from_eof().await,
                lspf::Outcome::TransportClosed,
            )
        }
        CleanupTrigger::WriterFailure => {
            connection.fail_next_write();
            connection.outbound_control.resume();
            (
                await_outcome(connection.serving, "writer failure").await,
                lspf::Outcome::WriterFailed,
            )
        }
        CleanupTrigger::ShutdownThenExit => {
            connection.send(request(4, "shutdown", Value::Null));
            connection.outbound_control.resume();
            receive_id(&mut connection.outgoing, 4).await;
            receive_id(&mut connection.outgoing, 3).await;
            connection.send(notification("exit", Value::Null));
            (
                await_outcome(connection.serving, "shutdown and exit").await,
                lspf::Outcome::Exit { code: 0 },
            )
        }
    };
    assert_eq!(outcome, expected_outcome);

    assert!(matches!(
        pending.await.expect("pending task did not panic"),
        Err(ClientError::Cancelled)
    ));
    wait_for_handler_task_count(&state.handler_tasks, 0).await;
    assert!(
        documents
            .get(&"file:///retained.txt".parse().unwrap())
            .is_none(),
        "close cleared the retained Documents view"
    );
    assert!(matches!(
        progress.report(None, Some(50)),
        Err(ClientError::Progress(lspf::ProgressError::UnknownToken))
    ));

    let events = logs.events();
    assert_resource_events_stay_within_limits(&events);
    for resource in [
        "inbound_requests",
        "outbound_queue",
        "documents",
        "pending_requests",
    ] {
        assert_resource_finished_at_zero(&events, resource);
    }
    assert_eq!(
        completion_count(&events, "inbound", CaptureHandles::METHOD, "2"),
        1
    );
    assert_eq!(completion_count(&events, "inbound", Stalls::METHOD, "3"), 1);
    assert_eq!(
        completion_count(&events, "outbound", PeerEcho::METHOD, &peer_id_text),
        1
    );
    if matches!(trigger, CleanupTrigger::ShutdownThenExit) {
        assert_eq!(completion_count(&events, "inbound", "shutdown", "4"), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn eof_clears_every_connection_resource_exactly_once() {
    assert_close_clears_every_connection_resource(CleanupTrigger::Eof).await;
}

#[tokio::test(flavor = "current_thread")]
async fn writer_failure_clears_every_connection_resource_exactly_once() {
    assert_close_clears_every_connection_resource(CleanupTrigger::WriterFailure).await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_then_exit_clears_every_connection_resource_exactly_once() {
    assert_close_clears_every_connection_resource(CleanupTrigger::ShutdownThenExit).await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn handler_timeout_completes_the_request_exactly_once() {
    let logs = LogBuffer::default();
    let _guard = capture_traces(&logs);
    let server = Server::builder(())
        .resource_policy(ResourcePolicy {
            handler_timeout: Duration::from_secs(5),
            ..ResourcePolicy::default()
        })
        .request::<Never, _, _>(never)
        .build()
        .unwrap();
    let mut connection = TestConnection::start(server).await;

    connection.send(request(2, Never::METHOD, Value::Null));
    tokio::time::advance(Duration::from_secs(5)).await;
    receive_id(&mut connection.outgoing, 2).await;
    connection.stop().await;

    let events = logs.events();
    assert_eq!(completion_count(&events, "inbound", Never::METHOD, "2"), 1);
    assert!(events.iter().any(|event| {
        event["message"] == "request completed"
            && event["direction"] == "inbound"
            && event["method"] == Never::METHOD
            && event["request_id"] == "2"
            && event["completion"] == "deadline_expired"
    }));
}
