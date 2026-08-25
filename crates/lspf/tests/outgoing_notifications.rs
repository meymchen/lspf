//! Typed outgoing notification helpers on `Client` (issue #102).
//!
//! These tests exercise the helpers over an in-memory transport, capturing the
//! connection's `Client` from inside a handler and observing the exact wire
//! method and parameter shape against the fixtures under `tests/fixtures/`.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};

use bytes::Bytes;
use lspf::types::notification::{DidCloseTextDocument, DidOpenTextDocument};
use lspf::types::request::Request;
use lspf::types::{
    Diagnostic, DiagnosticSeverity, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    LogMessageParams, LogTraceParams, MessageType, NumberOrString, Position, ProgressParams,
    ProgressParamsValue, PublishDiagnosticsParams, Range, ShowMessageParams, Uri, WorkDoneProgress,
    WorkDoneProgressBegin,
};
use lspf::{
    CancellationToken, Client, ClientError, Context, LspError, RawMessage, RequestId, Server,
    TelemetryEventParams, Transport, TransportError, TransportReader, TransportWriter,
};
use serde_json::json;
use tokio::sync::mpsc;

const PUBLISH_DIAGNOSTICS_FIXTURE: &str =
    include_str!("fixtures/publish_diagnostics_notification.json");
const SHOW_MESSAGE_FIXTURE: &str = include_str!("fixtures/show_message_notification.json");
const LOG_MESSAGE_FIXTURE: &str = include_str!("fixtures/log_message_notification.json");
const LOG_TRACE_FIXTURE: &str = include_str!("fixtures/log_trace_notification.json");
const TELEMETRY_EVENT_OBJECT_FIXTURE: &str = include_str!("fixtures/telemetry_event_object.json");
const TELEMETRY_EVENT_ARRAY_FIXTURE: &str = include_str!("fixtures/telemetry_event_array.json");
const PROGRESS_FIXTURE: &str = include_str!("fixtures/progress_notification.json");

// --- Registrations -----------------------------------------------------------

struct AppState {
    client: Arc<Mutex<Option<Client>>>,
}

enum CaptureClient {}

impl Request for CaptureClient {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "test/capture-client";
}

enum TraceLevel {}

impl Request for TraceLevel {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "test/trace-level";
}

enum CtxPublish {}

impl Request for CtxPublish {
    type Params = PublishDiagnosticsParams;
    type Result = String;
    const METHOD: &'static str = "test/ctx-publish";
}

async fn capture_client(
    state: Arc<AppState>,
    ctx: Context,
    _params: String,
    _ct: CancellationToken,
) -> Result<String, LspError> {
    *state.client.lock().unwrap() = Some(ctx.client());
    Ok("captured".to_string())
}

async fn trace_level(
    _state: Arc<AppState>,
    ctx: Context,
    _params: String,
    _ct: CancellationToken,
) -> Result<String, LspError> {
    let level = match ctx.workspace().trace() {
        lspf::types::TraceValue::Off => "off",
        lspf::types::TraceValue::Messages => "messages",
        lspf::types::TraceValue::Verbose => "verbose",
    };
    Ok(level.to_string())
}

async fn ctx_publish(
    _state: Arc<AppState>,
    ctx: Context,
    params: PublishDiagnosticsParams,
    _ct: CancellationToken,
) -> Result<String, LspError> {
    // The legacy convenience surface propagates enqueue failures instead of
    // swallowing them.
    ctx.publish_diagnostics(params)
        .map_err(LspError::internal)?;
    Ok("published".to_string())
}

async fn noop_open(_state: Arc<AppState>, _ctx: Context, _params: DidOpenTextDocumentParams) {}

async fn noop_close(_state: Arc<AppState>, _ctx: Context, _params: DidCloseTextDocumentParams) {}

fn server(state: &Arc<Mutex<Option<Client>>>) -> Server<AppState> {
    Server::builder(AppState {
        client: Arc::clone(state),
    })
    .request::<CaptureClient, _, _>(capture_client)
    .request::<TraceLevel, _, _>(trace_level)
    .request::<CtxPublish, _, _>(ctx_publish)
    .notification::<DidOpenTextDocument, _, _>(noop_open)
    .notification::<DidCloseTextDocument, _, _>(noop_close)
    .build()
    .expect("the outgoing-notification server builds")
}

// --- Harness -----------------------------------------------------------------

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

async fn start(server: Server<AppState>) -> Session {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let serve = tokio::spawn(server.serve(ChannelTransport { in_rx, out_tx }));
    Session {
        in_tx,
        out_rx,
        serve,
    }
}

/// Send a request and await its correlated response, synchronizing the test
/// with the read loop: every notification sent before the request has been
/// processed by the time the response arrives.
async fn request_and_sync(session: &mut Session, message: RawMessage) -> RawMessage {
    let id = message.id().cloned().expect("a request carries an id");
    session.in_tx.send(message).unwrap();
    let response = receive(&mut session.out_rx).await;
    assert_eq!(response.id(), Some(&id));
    response
}

fn result_string(response: &RawMessage) -> String {
    match response {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => serde_json::from_slice(bytes).expect("the result decodes"),
        other => panic!("expected a success response, got {other:?}"),
    }
}

/// The connection's current trace level, observed through a handler's
/// workspace — proof of what `Client::log_trace` gates on.
async fn trace_level_probe(session: &mut Session, id: i32) -> String {
    let response = request_and_sync(session, request(id, TraceLevel::METHOD, json!("probe"))).await;
    result_string(&response)
}

async fn set_trace(session: &mut Session, id: i32, value: &str) {
    session
        .in_tx
        .send(notification("$/setTrace", json!({ "value": value })))
        .unwrap();
    assert_eq!(trace_level_probe(session, id).await, value);
}

/// A session that ran initialize and captured its connection `Client`.
async fn initialized_session() -> (Session, Arc<Mutex<Option<Client>>>) {
    let captured = Arc::new(Mutex::new(None));
    let mut session = start(server(&captured)).await;
    request_and_sync(
        &mut session,
        request(
            1,
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        ),
    )
    .await;
    request_and_sync(&mut session, request(2, CaptureClient::METHOD, json!("x"))).await;
    (session, captured)
}

fn take_client(captured: &Arc<Mutex<Option<Client>>>) -> Client {
    captured
        .lock()
        .unwrap()
        .clone()
        .expect("the handler captured its connection Client")
}

async fn finish(session: Session) {
    session.in_tx.send(exit()).unwrap();
    session
        .serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
}

fn assert_wire_fixture(message: &RawMessage, fixture: &str) {
    let RawMessage::Notification { method, params } = message else {
        panic!("expected a notification, got {message:?}")
    };
    let wire = json!({
        "method": method.as_ref(),
        "params": serde_json::from_slice::<serde_json::Value>(params)
            .expect("the params are valid JSON"),
    });
    let expected: serde_json::Value =
        serde_json::from_str(fixture).expect("the fixture is valid JSON");
    assert_eq!(wire, expected, "the wire shape must match the fixture");
}

// --- Params used across tests ------------------------------------------------

fn diagnostics_params() -> PublishDiagnosticsParams {
    PublishDiagnosticsParams {
        uri: Uri::from_str("file:///diagnostics.rs").expect("the URI parses"),
        diagnostics: vec![Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 5)),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("lspf-test".to_string()),
            message: "boom".to_string(),
            ..Diagnostic::default()
        }],
        version: Some(7),
    }
}

fn show_message_params() -> ShowMessageParams {
    ShowMessageParams {
        typ: MessageType::WARNING,
        message: "disk almost full".to_string(),
    }
}

fn log_message_params() -> LogMessageParams {
    LogMessageParams {
        typ: MessageType::INFO,
        message: "indexed 42 files".to_string(),
    }
}

fn log_trace_params() -> LogTraceParams {
    LogTraceParams {
        message: "resolved import".to_string(),
        verbose: Some("walked the import graph".to_string()),
    }
}

fn telemetry_object_params() -> TelemetryEventParams {
    TelemetryEventParams::from(
        json!({"eventName": "build", "durationMs": 42})
            .as_object()
            .expect("the telemetry payload is an object")
            .clone(),
    )
}

fn telemetry_array_params() -> TelemetryEventParams {
    TelemetryEventParams::from(
        json!(["build", 42])
            .as_array()
            .expect("the telemetry payload is an array")
            .clone(),
    )
}

fn progress_params() -> ProgressParams {
    ProgressParams {
        token: NumberOrString::String("build-1".to_string()),
        value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(WorkDoneProgressBegin {
            title: "Building".to_string(),
            cancellable: None,
            message: Some("crates".to_string()),
            percentage: Some(10),
        })),
    }
}

// --- Tests -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_diagnostics_matches_the_wire_fixture_and_preserves_the_version() {
    let (mut session, captured) = initialized_session().await;
    let client = take_client(&captured);

    client
        .publish_diagnostics(diagnostics_params())
        .expect("a successful enqueue returns Ok(())");

    assert_wire_fixture(
        &receive(&mut session.out_rx).await,
        PUBLISH_DIAGNOSTICS_FIXTURE,
    );
    finish(session).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_publish_diagnostics_returns_the_enqueue_result() {
    let (mut session, _captured) = initialized_session().await;

    // The notification is enqueued before the handler's response, and the
    // outbound channel is FIFO.
    session
        .in_tx
        .send(request(
            3,
            CtxPublish::METHOD,
            serde_json::to_value(diagnostics_params()).unwrap(),
        ))
        .unwrap();
    let notification = receive(&mut session.out_rx).await;
    assert_wire_fixture(&notification, PUBLISH_DIAGNOSTICS_FIXTURE);
    let response = receive(&mut session.out_rx).await;
    assert_eq!(response.id(), Some(&RequestId::Number(3)));
    assert_eq!(result_string(&response), "published");
    finish(session).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn show_message_matches_the_wire_fixture() {
    let (mut session, captured) = initialized_session().await;
    let client = take_client(&captured);

    client
        .show_message(show_message_params())
        .expect("a successful enqueue returns Ok(())");

    assert_wire_fixture(&receive(&mut session.out_rx).await, SHOW_MESSAGE_FIXTURE);
    finish(session).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn log_message_matches_the_wire_fixture() {
    let (mut session, captured) = initialized_session().await;
    let client = take_client(&captured);

    client
        .log_message(log_message_params())
        .expect("a successful enqueue returns Ok(())");

    assert_wire_fixture(&receive(&mut session.out_rx).await, LOG_MESSAGE_FIXTURE);
    finish(session).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn telemetry_event_matches_the_wire_fixtures() {
    let (mut session, captured) = initialized_session().await;
    let client = take_client(&captured);

    client
        .telemetry_event(telemetry_object_params())
        .expect("an object payload enqueues");
    assert_wire_fixture(
        &receive(&mut session.out_rx).await,
        TELEMETRY_EVENT_OBJECT_FIXTURE,
    );

    client
        .telemetry_event(telemetry_array_params())
        .expect("an array payload enqueues");
    assert_wire_fixture(
        &receive(&mut session.out_rx).await,
        TELEMETRY_EVENT_ARRAY_FIXTURE,
    );

    finish(session).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_matches_the_wire_fixture() {
    let (mut session, captured) = initialized_session().await;
    let client = take_client(&captured);

    client
        .progress(progress_params())
        .expect("a successful enqueue returns Ok(())");

    assert_wire_fixture(&receive(&mut session.out_rx).await, PROGRESS_FIXTURE);
    finish(session).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn log_trace_gates_on_the_connection_trace_level_without_changing_it() {
    let (mut session, captured) = initialized_session().await;
    let client = take_client(&captured);

    // The initial level is Off: nothing is enqueued, and the no-op is Ok.
    client
        .log_trace(log_trace_params())
        .expect("an Off trace level is a silent no-op");
    assert!(
        session.out_rx.try_recv().is_err(),
        "an Off trace level enqueues nothing"
    );

    // Messages sends the params as provided.
    set_trace(&mut session, 3, "messages").await;
    client
        .log_trace(log_trace_params())
        .expect("a Messages trace level enqueues");
    assert_wire_fixture(&receive(&mut session.out_rx).await, LOG_TRACE_FIXTURE);
    // Sending never changes the level.
    assert_eq!(trace_level_probe(&mut session, 4).await, "messages");

    // Verbose sends too, still without changing the level.
    set_trace(&mut session, 5, "verbose").await;
    client
        .log_trace(log_trace_params())
        .expect("a Verbose trace level enqueues");
    assert_wire_fixture(&receive(&mut session.out_rx).await, LOG_TRACE_FIXTURE);
    assert_eq!(trace_level_probe(&mut session, 6).await, "verbose");

    finish(session).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_helper_reports_a_closed_queue_as_client_error() {
    let (mut session, captured) = initialized_session().await;
    let client = take_client(&captured);
    // Raise the level so log_trace attempts an enqueue instead of no-oping.
    set_trace(&mut session, 3, "messages").await;
    finish(session).await;

    assert!(matches!(
        client.publish_diagnostics(diagnostics_params()),
        Err(ClientError::OutboundClosed)
    ));
    assert!(matches!(
        client.show_message(show_message_params()),
        Err(ClientError::OutboundClosed)
    ));
    assert!(matches!(
        client.log_message(log_message_params()),
        Err(ClientError::OutboundClosed)
    ));
    assert!(matches!(
        client.log_trace(log_trace_params()),
        Err(ClientError::OutboundClosed)
    ));
    assert!(matches!(
        client.telemetry_event(telemetry_object_params()),
        Err(ClientError::OutboundClosed)
    ));
    assert!(matches!(
        client.progress(progress_params()),
        Err(ClientError::OutboundClosed)
    ));
}

/// Captures `tracing` events with their rendered fields, so tests can assert
/// what the helpers emit locally.
#[derive(Clone, Default)]
struct EventCapture {
    events: Arc<Mutex<Vec<(tracing::Level, String)>>>,
}

static TRACING_INTEREST: OnceLock<tracing::Dispatch> = OnceLock::new();

fn keep_tracing_interest() {
    // Callsite interest is cached process-wide, while scoped subscribers are
    // thread-local. Keep one dispatch alive for this test binary so parallel
    // tests cannot leave the queue callsites permanently disabled.
    TRACING_INTEREST.get_or_init(|| tracing::Dispatch::new(tracing_subscriber::registry()));
}

struct FieldVisitor(String);

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        self.0.push_str(&format!("{}={value:?}", field.name()));
    }
}

impl<S> tracing_subscriber::Layer<S> for EventCapture
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor(String::new());
        event.record(&mut visitor);
        self.events
            .lock()
            .unwrap()
            .push((*event.metadata().level(), visitor.0));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn helper_failures_emit_a_tracing_event_without_suppressing_the_error() {
    use tracing_subscriber::layer::SubscriberExt;

    keep_tracing_interest();
    let (mut session, captured) = initialized_session().await;
    let client = take_client(&captured);
    let capture = EventCapture::default();

    // A successful log message records only queue accounting locally; its
    // protocol payload is not duplicated into the tracing stream.
    tracing::subscriber::with_default(tracing_subscriber::registry().with(capture.clone()), || {
        client
            .log_message(log_message_params())
            .expect("a successful enqueue returns Ok(())");
    });
    {
        let events = capture.events.lock().unwrap();
        assert!(events.iter().any(|(level, fields)| {
            *level == tracing::Level::TRACE
                && fields.contains("resource=\"outbound_queue\"")
                && fields.contains("resource_action=\"admit\"")
        }));
        assert!(
            events
                .iter()
                .all(|(_, fields)| !fields.contains("indexed 42 files")),
            "log_message must not echo the payload into local tracing; got {events:?}"
        );
    }
    assert_wire_fixture(&receive(&mut session.out_rx).await, LOG_MESSAGE_FIXTURE);

    // A failed enqueue emits a tracing event naming the wire method, and the
    // error is still returned to the caller.
    finish(session).await;
    let result = tracing::subscriber::with_default(
        tracing_subscriber::registry().with(capture.clone()),
        || client.show_message(show_message_params()),
    );
    assert!(matches!(result, Err(ClientError::OutboundClosed)));
    let events = capture.events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|(level, fields)| *level == tracing::Level::WARN
                && fields.contains("window/showMessage")),
        "a failed helper emits a WARN event naming the method; got {events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn did_close_never_emits_an_automatic_publish_diagnostics() {
    let captured = Arc::new(Mutex::new(None));
    let mut session = start(server(&captured)).await;
    request_and_sync(
        &mut session,
        request(
            1,
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        ),
    )
    .await;

    session
        .in_tx
        .send(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///watched.rs",
                    "languageId": "rust",
                    "version": 1,
                    "text": "fn main() {}"
                }
            }),
        ))
        .unwrap();
    session
        .in_tx
        .send(notification(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": "file:///watched.rs" } }),
        ))
        .unwrap();
    // The probe synchronizes with the read loop: open and close are fully
    // processed before the response comes back.
    request_and_sync(&mut session, request(2, CaptureClient::METHOD, json!("x"))).await;

    session.in_tx.send(exit()).unwrap();
    session
        .serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");

    while let Ok(message) = session.out_rx.try_recv() {
        assert!(
            !matches!(&message, RawMessage::Notification { method, .. } if method == "textDocument/publishDiagnostics"),
            "didClose must not emit an automatic publishDiagnostics"
        );
    }
}
