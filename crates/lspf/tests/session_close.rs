//! One convergent session close for shutdown, exit, EOF, and writer failure
//! (issue #48, ADR 0018).
//!
//! Every test drives the public `Server::serve` transport seam and asserts on
//! the returned `Outcome`. Channels and notifications establish the relevant
//! ordering; no assertion depends on a scheduler delay.

use std::borrow::Cow;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use lspf::types::notification::Notification;
use lspf::types::request::Request;
use lspf::{
    CancellationToken, Client, ClientError, Context, LspError, Outcome, RawMessage, RequestId,
    Server, Transport, TransportError, TransportReader, TransportWriter,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

// --- Marker types ------------------------------------------------------------

enum Slow {}

impl Request for Slow {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "test/slow";
}

enum Capture {}

impl Request for Capture {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "test/capture";
}

enum Echo {}

impl Request for Echo {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "test/echo";
}

enum Ping {}

impl Notification for Ping {
    type Params = Value;
    const METHOD: &'static str = "test/ping";
}

/// The server-to-client request the peer never answers, so only the engine's
/// close operation can resolve it.
enum NeverAnswered {}

impl Request for NeverAnswered {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "client/never-answered";
}

// --- Server under test -------------------------------------------------------

/// Set when a long-running handler's future is finally dropped, which the
/// engine's abort-then-join guarantees before serving returns.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct AppState {
    /// Signals that `test/slow` has begun, so a test can order `shutdown` or
    /// `exit` after a request is genuinely in flight.
    started: Mutex<Option<mpsc::UnboundedSender<()>>>,
    /// Set once the in-flight `test/slow` future is dropped.
    handler_dropped: Arc<AtomicBool>,
    /// A `Client` handle lifted out of a handler, so a test can hold an
    /// outbound request pending from outside the engine's task group. A handler
    /// cannot return one through its result, which crosses the wire.
    client: Arc<Mutex<Option<Client>>>,
    /// Set if the `test/ping` notification ever reaches user dispatch.
    notification_dispatched: Arc<AtomicBool>,
}

async fn slow(
    state: Arc<AppState>,
    _ctx: Context,
    _params: Value,
    _ct: CancellationToken,
) -> Result<Value, LspError> {
    let _dropped = DropFlag(Arc::clone(&state.handler_dropped));
    if let Some(started) = state.started.lock().unwrap().as_ref() {
        let _ = started.send(());
    }
    // Deliberately ignores its cancellation token: ending a long-running
    // request is the engine's job, not the handler's.
    std::future::pending().await
}

async fn capture(
    state: Arc<AppState>,
    ctx: Context,
    _params: Value,
    _ct: CancellationToken,
) -> Result<Value, LspError> {
    *state.client.lock().unwrap() = Some(ctx.client());
    Ok(json!(null))
}

async fn echo(
    _state: Arc<AppState>,
    _ctx: Context,
    params: Value,
    _ct: CancellationToken,
) -> Result<Value, LspError> {
    Ok(params)
}

async fn ping(state: Arc<AppState>, _ctx: Context, _params: Value) {
    state.notification_dispatched.store(true, Ordering::SeqCst);
}

fn server(state: AppState) -> Server<AppState> {
    Server::builder(state)
        .request::<Slow, _, _>(slow)
        .request::<Capture, _, _>(capture)
        .request::<Echo, _, _>(echo)
        .notification::<Ping, _, _>(ping)
        .build()
        .expect("server builds")
}

// --- In-memory transport -----------------------------------------------------

/// Never fail: the writer forwards every message.
const ALWAYS_WRITES: usize = usize::MAX;

struct ChannelTransport {
    incoming: mpsc::UnboundedReceiver<RawMessage>,
    outgoing: mpsc::UnboundedSender<RawMessage>,
    fail_after: usize,
    malformed_read: bool,
}

/// Ends either with plain EOF or, when `malformed`, with a reader transport
/// error — the two endings the engine must tell apart.
struct ChannelReader {
    incoming: mpsc::UnboundedReceiver<RawMessage>,
    malformed: bool,
}

/// Forwards messages until `fail_after` of them have been written, then fails
/// every send. The adapter only reports the failure; ADR 0018 leaves every
/// registry, task, and queue to the engine's close operation.
struct ChannelWriter {
    outgoing: mpsc::UnboundedSender<RawMessage>,
    fail_after: usize,
    sent: usize,
}

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            ChannelReader {
                incoming: self.incoming,
                malformed: self.malformed_read,
            },
            ChannelWriter {
                outgoing: self.outgoing,
                fail_after: self.fail_after,
                sent: 0,
            },
        )
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        match self.incoming.recv().await {
            Some(message) => Ok(message),
            None if self.malformed => Err(TransportError::Malformed("truncated frame".to_string())),
            None => Err(TransportError::Closed),
        }
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        if self.sent >= self.fail_after {
            return Err(TransportError::Io(io::Error::other("writer failed")));
        }
        self.sent += 1;
        self.outgoing
            .send(message)
            .map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

// --- Helpers -----------------------------------------------------------------

type Serving = tokio::task::JoinHandle<lspf::Result<Outcome>>;

fn start(
    state: AppState,
    fail_after: usize,
) -> (
    mpsc::UnboundedSender<RawMessage>,
    mpsc::UnboundedReceiver<RawMessage>,
    Serving,
) {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let serving = tokio::spawn(server(state).serve(ChannelTransport {
        incoming: in_rx,
        outgoing: out_tx,
        fail_after,
        malformed_read: false,
    }));
    (in_tx, out_rx, serving)
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

fn response(id: i32, result: Value) -> RawMessage {
    RawMessage::Response {
        id: RequestId::Number(id),
        result: Ok(Bytes::from(serde_json::to_vec(&result).unwrap())),
    }
}

fn initialize(id: i32) -> RawMessage {
    request(
        id,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    )
}

fn exit() -> RawMessage {
    notification("exit", json!(null))
}

async fn recv(out: &mut mpsc::UnboundedReceiver<RawMessage>) -> RawMessage {
    tokio::time::timeout(std::time::Duration::from_secs(2), out.recv())
        .await
        .expect("message within 2s")
        .expect("outgoing channel open")
}

/// The `(id, error code)` of an error response, panicking on any other message.
fn error_of(message: &RawMessage) -> (RequestId, i32) {
    match message {
        RawMessage::Response {
            id,
            result: Err(error),
        } => (id.clone(), error.code),
        other => panic!("expected an error response, got {other:?}"),
    }
}

async fn served(serving: Serving) -> lspf::Result<Outcome> {
    tokio::time::timeout(std::time::Duration::from_secs(5), serving)
        .await
        .expect("serving returned within 5s rather than hanging on close")
        .expect("serving did not panic")
}

/// Drive the connection to the running state.
async fn initialized(
    in_tx: &mpsc::UnboundedSender<RawMessage>,
    out: &mut mpsc::UnboundedReceiver<RawMessage>,
) {
    in_tx.send(initialize(1)).unwrap();
    let response = recv(out).await;
    assert_eq!(response.id(), Some(&RequestId::Number(1)));
}

// --- Tests -------------------------------------------------------------------

/// Reader EOF alone closes the session and reports the transport as closed.
/// After serving returns, the writer task has ended too — its half of the
/// transport is dropped, so the outgoing channel is closed rather than left
/// alive by a detached task.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reader_eof_closes_the_session_and_joins_the_writer() {
    let (in_tx, mut out_rx, serving) = start(AppState::default(), ALWAYS_WRITES);
    initialized(&in_tx, &mut out_rx).await;

    drop(in_tx);

    assert_eq!(
        served(serving).await.expect("EOF is not a transport error"),
        Outcome::TransportClosed
    );
    assert!(
        out_rx.recv().await.is_none(),
        "the writer task ended and dropped the transport's writer half"
    );
}

/// A writer send failure ends the session on its own: the read-loop is parked
/// on a peer that never speaks again, yet the engine still closes and reports
/// the writer as the cause.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writer_failure_closes_the_session_without_any_further_input() {
    // The writer fails on its very first send, which is the initialize response.
    let (in_tx, _out_rx, serving) = start(AppState::default(), 0);

    in_tx.send(initialize(1)).unwrap();

    // The peer never closes its half, so only the close signal can wake the
    // read-loop out of `recv`.
    assert!(!in_tx.is_closed(), "the peer half is still open");
    assert_eq!(
        served(serving)
            .await
            .expect("a writer failure is reported as an outcome, not a reader error"),
        Outcome::WriterFailed
    );
}

/// Reader EOF racing a writer send failure runs one close path and reports one
/// cause. Whichever requester wins, the other observes the same close: serving
/// resolves once, to one of the two endings, and the writer task is joined.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_eof_and_writer_failure_report_one_close_cause() {
    // Let the initialize response through, then fail every later write.
    let (in_tx, mut out_rx, serving) = start(AppState::default(), 1);
    initialized(&in_tx, &mut out_rx).await;

    // The echo response fails to write while the reader reaches EOF.
    in_tx.send(request(2, "test/echo", json!("hi"))).unwrap();
    drop(in_tx);

    let outcome = served(serving)
        .await
        .expect("neither ending is a reader transport error");
    assert!(
        matches!(outcome, Outcome::TransportClosed | Outcome::WriterFailed),
        "exactly one of the two racing causes is reported, got {outcome:?}"
    );
    assert!(
        out_rx.recv().await.is_none(),
        "the one close operation joined the writer task"
    );
}

/// Session close resolves every pending server-to-client request with the
/// framework-owned session-closed error, so no `Client` caller is left waiting.
///
/// The caller here lives outside the engine's task group, so the resolution is
/// observed directly rather than inferred from the absence of a hang.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_resolves_every_pending_client_request() {
    let captured = Arc::new(Mutex::new(None));
    let state = AppState {
        client: Arc::clone(&captured),
        ..AppState::default()
    };
    let (in_tx, mut out_rx, serving) = start(state, ALWAYS_WRITES);
    initialized(&in_tx, &mut out_rx).await;

    // Lift a Client handle out of a handler so the pending request survives
    // independently of the engine's task group. The response proves the handler
    // ran, so the handle is already stored.
    in_tx.send(request(2, "test/capture", json!(null))).unwrap();
    recv(&mut out_rx).await;
    let client = captured.lock().unwrap().take().expect("client captured");

    let pending = tokio::spawn(async move { client.request::<NeverAnswered>(json!({})).await });

    // The outbound request reached the wire, so it is registered and pending.
    let outbound = recv(&mut out_rx).await;
    assert_eq!(outbound.method(), Some("client/never-answered"));

    drop(in_tx);

    assert_eq!(
        served(serving).await.expect("EOF is not a transport error"),
        Outcome::TransportClosed
    );

    let error = pending
        .await
        .expect("the pending caller did not panic")
        .expect_err("close resolves the request with an error");
    assert!(
        matches!(error, ClientError::Cancelled),
        "expected the framework-owned session-closed error, got {error:?}"
    );
}

/// A successful `shutdown` answers itself, then cancels every other in-flight
/// request, refuses later requests with `InvalidRequest`, and drops user
/// notifications. `exit` afterwards reports code 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_answers_itself_then_cancels_and_refuses_later_work() {
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let dispatched = Arc::new(AtomicBool::new(false));
    let state = AppState {
        started: Mutex::new(Some(started_tx)),
        notification_dispatched: Arc::clone(&dispatched),
        ..AppState::default()
    };
    let (in_tx, mut out_rx, serving) = start(state, ALWAYS_WRITES);
    initialized(&in_tx, &mut out_rx).await;

    // A long-running request is genuinely in flight before shutdown arrives.
    in_tx.send(request(2, "test/slow", json!(null))).unwrap();
    started_rx.recv().await.expect("the slow handler started");

    in_tx.send(request(3, "shutdown", json!(null))).unwrap();

    // The shutdown request sends its own response first.
    let shutdown_response = recv(&mut out_rx).await;
    assert!(
        matches!(
            &shutdown_response,
            RawMessage::Response { id, result: Ok(_) } if *id == RequestId::Number(3)
        ),
        "shutdown answers itself, got {shutdown_response:?}"
    );

    // Only then is the other in-flight request cancelled.
    assert_eq!(
        error_of(&recv(&mut out_rx).await),
        (RequestId::Number(2), -32800),
        "a successful shutdown cancels the rest of the in-flight requests"
    );

    // Later requests are invalid, and user notifications are no longer processed.
    in_tx.send(notification("test/ping", json!(null))).unwrap();
    in_tx.send(request(4, "test/echo", json!(null))).unwrap();
    assert_eq!(
        error_of(&recv(&mut out_rx).await),
        (RequestId::Number(4), -32600),
        "requests after shutdown are invalid"
    );
    assert!(
        !dispatched.load(Ordering::SeqCst),
        "a user notification after shutdown never reaches user dispatch"
    );

    in_tx.send(exit()).unwrap();
    assert_eq!(
        served(serving)
            .await
            .expect("exit ends the session cleanly"),
        Outcome::Exit { code: 0 },
        "exit after a successful shutdown reports code 0"
    );
}

/// Responses to outbound requests are still correlated after `shutdown` — they
/// are completion traffic, not new user work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_responses_still_correlate_after_shutdown() {
    let captured = Arc::new(Mutex::new(None));
    let state = AppState {
        client: Arc::clone(&captured),
        ..AppState::default()
    };
    let (in_tx, mut out_rx, serving) = start(state, ALWAYS_WRITES);
    initialized(&in_tx, &mut out_rx).await;

    in_tx.send(request(2, "test/capture", json!(null))).unwrap();
    recv(&mut out_rx).await;
    let client = captured.lock().unwrap().take().expect("client captured");

    let pending = tokio::spawn(async move { client.request::<NeverAnswered>(json!({})).await });
    let outbound = recv(&mut out_rx).await;
    let outbound_id = match outbound.id() {
        Some(RequestId::Number(id)) => *id,
        other => panic!("expected a numeric outbound id, got {other:?}"),
    };

    in_tx.send(request(3, "shutdown", json!(null))).unwrap();
    recv(&mut out_rx).await;

    // The peer answers the outbound request after shutdown; it must complete.
    in_tx
        .send(response(outbound_id, json!("answered")))
        .unwrap();
    let answered = pending
        .await
        .expect("the pending caller did not panic")
        .expect("a response after shutdown still completes its request");
    assert_eq!(answered, json!("answered"));

    in_tx.send(exit()).unwrap();
    assert_eq!(
        served(serving)
            .await
            .expect("exit ends the session cleanly"),
        Outcome::Exit { code: 0 }
    );
}

/// `exit` without a preceding `shutdown` ends a long-running handler and
/// reports code 1. The handler's future is dropped before serving returns,
/// which is what abort-then-join guarantees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_ends_a_long_running_handler_and_reports_code_one() {
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let dropped = Arc::new(AtomicBool::new(false));
    let state = AppState {
        started: Mutex::new(Some(started_tx)),
        handler_dropped: Arc::clone(&dropped),
        ..AppState::default()
    };
    let (in_tx, mut out_rx, serving) = start(state, ALWAYS_WRITES);
    initialized(&in_tx, &mut out_rx).await;

    in_tx.send(request(2, "test/slow", json!(null))).unwrap();
    started_rx.recv().await.expect("the slow handler started");

    in_tx.send(exit()).unwrap();

    assert_eq!(
        served(serving)
            .await
            .expect("exit ends the session cleanly"),
        Outcome::Exit { code: 1 },
        "exit without shutdown reports code 1"
    );
    assert!(
        dropped.load(Ordering::SeqCst),
        "the long-running handler was aborted and joined before serving returned"
    );
}

/// The custom-transport entry point reports every ending to its caller and
/// terminates nothing, so one process can serve connection after connection —
/// including the `exit` codes a server binary would turn into a process
/// disposition. Each connection needs its own `Server`; connection state is
/// never shared between them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consecutive_connections_report_their_codes_without_ending_the_process() {
    // First connection: `exit` with no `shutdown` — the ending a binary maps to
    // process exit code 1.
    let (in_tx, mut out_rx, serving) = start(AppState::default(), ALWAYS_WRITES);
    initialized(&in_tx, &mut out_rx).await;
    in_tx.send(exit()).unwrap();
    assert_eq!(
        served(serving).await.expect("the first connection ended"),
        Outcome::Exit { code: 1 }
    );

    // The process is still here to serve a second connection from a second
    // `Server`, which reaches its own, independent ending.
    let (in_tx, mut out_rx, serving) = start(AppState::default(), ALWAYS_WRITES);
    initialized(&in_tx, &mut out_rx).await;
    in_tx.send(request(2, "shutdown", json!(null))).unwrap();
    assert_eq!(recv(&mut out_rx).await.id(), Some(&RequestId::Number(2)));
    in_tx.send(exit()).unwrap();
    assert_eq!(
        served(serving).await.expect("the second connection ended"),
        Outcome::Exit { code: 0 },
        "the second connection reports its own outcome, unaffected by the first"
    );
}

/// A reader transport error runs the same close operation but, unlike EOF, is
/// reported as an error rather than an outcome.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reader_transport_error_closes_the_session_and_is_reported() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let serving = tokio::spawn(server(AppState::default()).serve(ChannelTransport {
        incoming: in_rx,
        outgoing: out_tx,
        fail_after: ALWAYS_WRITES,
        malformed_read: true,
    }));

    in_tx.send(initialize(1)).unwrap();
    recv(&mut out_rx).await;

    drop(in_tx);

    let error = served(serving)
        .await
        .expect_err("a reader failure is an error, not an outcome");
    assert!(
        matches!(
            error,
            lspf::Error::Transport(TransportError::Malformed(message)) if message == "truncated frame"
        ),
        "the reader's own error is what serving reports"
    );
    assert!(
        out_rx.recv().await.is_none(),
        "the one close operation joined the writer task"
    );
}

/// A failed initialize transaction sends its fixed error and then takes the
/// same close path, reporting its own outcome instead of terminating the
/// process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_initialize_transaction_closes_the_session() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let server = Server::builder(())
        .on_initialize(|_state: Arc<()>, _ctx, _params, _ct| async {
            Err(LspError::internal("no"))
        })
        .build()
        .expect("server builds");
    let serving = tokio::spawn(server.serve(ChannelTransport {
        incoming: in_rx,
        outgoing: out_tx,
        fail_after: ALWAYS_WRITES,
        malformed_read: false,
    }));

    in_tx.send(initialize(1)).unwrap();
    assert_eq!(
        error_of(&recv(&mut out_rx).await),
        (RequestId::Number(1), -32603),
        "the failed transaction sends its fixed error before closing"
    );

    let outcome = served(serving)
        .await
        .expect("a failed initialize is an outcome, not a transport error");
    assert_eq!(outcome, Outcome::InitializeFailed);
    assert_eq!(outcome.code(), 1);
}
