//! Client cancellation of work-done progress (issue #110).
//!
//! `window/workDoneProgress/cancel` is a protocol-owned built-in: the engine
//! decodes it, resolves the token against the connection's progress registry,
//! and fires the matching handle's `CancellationToken` — without sending a
//! work-done end, which stays the application's decision. Unknown, malformed,
//! ended, and non-cancellable tokens are logged at debug level (unit-tested in
//! `src/engine.rs`) and leave the connection usable. These tests drive real
//! connections over an in-memory transport to prove the wire-level behavior:
//! cancellation before and after a report, the ignored-token cases, hook
//! ordering, and registry clearing at session close with independent
//! connections.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use lspf::types::WorkDoneProgressCancelParams;
use lspf::types::notification::{Notification, WorkDoneProgressCancel};
use lspf::types::request::Request;
use lspf::{
    ClientError, Context, LspError, ProgressError, ProgressHandle, ProgressOptions, RawMessage,
    RequestId, Server, Transport, TransportError, TransportReader, TransportWriter,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

// --- In-memory transport -----------------------------------------------------

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

// --- Application under test --------------------------------------------------

/// What the registered `window/workDoneProgress/cancel` hook observed, in
/// order: the decoded token and the stored handle's cancellation state at the
/// moment the hook ran.
#[derive(Debug, PartialEq, Eq)]
struct HookObservation {
    token: Value,
    cancelled: Option<bool>,
}

#[derive(Default)]
struct AppState {
    handle: Mutex<Option<ProgressHandle>>,
    hook_seen: Mutex<Vec<HookObservation>>,
}

enum BeginProgress {}

impl Request for BeginProgress {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "test/begin-progress";
}

enum ProbeProgress {}

impl Request for ProbeProgress {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "test/probe-progress";
}

enum ReportProgress {}

impl Request for ReportProgress {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "test/report-progress";
}

enum EndProgress {}

impl Request for EndProgress {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "test/end-progress";
}

fn report_outcome(result: Result<(), ClientError>) -> &'static str {
    match result {
        Ok(()) => "ok",
        Err(ClientError::Progress(ProgressError::Cancelled)) => "cancelled",
        Err(ClientError::Progress(ProgressError::UnknownToken)) => "unknown-token",
        Err(other) => panic!("unexpected report error: {other:?}"),
    }
}

/// Build the test server. With `with_hook`, a `window/workDoneProgress/cancel`
/// notification registration records the built-in's post-validation hook,
/// which appends one [`HookObservation`] per invocation.
fn server(state: Arc<AppState>, with_hook: bool) -> Server<()> {
    let begin_state = Arc::clone(&state);
    let probe_state = Arc::clone(&state);
    let report_state = Arc::clone(&state);
    let end_state = Arc::clone(&state);
    let builder = Server::builder(())
        .request::<BeginProgress, _, _>(move |_state: Arc<()>, ctx: Context, params: Value, _ct| {
            let state = Arc::clone(&begin_state);
            async move {
                let cancellable = params
                    .get("cancellable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let handle = ctx
                    .client()
                    .begin_progress(ProgressOptions::new("Indexing").cancellable(cancellable))
                    .await
                    .map_err(LspError::internal)?;
                let token = serde_json::to_value(handle.token()).unwrap();
                *state.handle.lock().unwrap() = Some(handle);
                Ok(json!({ "token": token }))
            }
        })
        .request::<ProbeProgress, _, _>(
            move |_state: Arc<()>, _ctx: Context, _params: Value, _ct| {
                let state = Arc::clone(&probe_state);
                async move {
                    let guard = state.handle.lock().unwrap();
                    match guard.as_ref() {
                        Some(handle) => Ok(json!({
                            "handle": true,
                            "cancelled": handle.cancellation_token().is_cancelled(),
                            "report": report_outcome(handle.report(None, None)),
                        })),
                        None => Ok(json!({ "handle": false })),
                    }
                }
            },
        )
        .request::<ReportProgress, _, _>(
            move |_state: Arc<()>, _ctx: Context, _params: Value, _ct| {
                let state = Arc::clone(&report_state);
                async move {
                    let guard = state.handle.lock().unwrap();
                    let handle = guard.as_ref().expect("progress begun");
                    Ok(json!({
                        "report": report_outcome(handle.report(Some("half".into()), Some(50))),
                    }))
                }
            },
        )
        .request::<EndProgress, _, _>(move |_state: Arc<()>, _ctx: Context, params: Value, _ct| {
            let state = Arc::clone(&end_state);
            async move {
                let handle = state.handle.lock().unwrap().take().expect("progress begun");
                let message = params
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                handle.end(message).map_err(LspError::internal)?;
                Ok(json!({ "end": "ok" }))
            }
        });
    if !with_hook {
        return builder.build().expect("server builds");
    }
    builder
        .notification::<WorkDoneProgressCancel, _, _>(
            move |_state: Arc<()>, _ctx: Context, params: WorkDoneProgressCancelParams| {
                let state = Arc::clone(&state);
                async move {
                    let cancelled = state
                        .handle
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|handle| handle.cancellation_token().is_cancelled());
                    state.hook_seen.lock().unwrap().push(HookObservation {
                        token: serde_json::to_value(params.token).unwrap(),
                        cancelled,
                    });
                }
            },
        )
        .build()
        .expect("a cancel-hook registration builds")
}

// --- Session helpers ---------------------------------------------------------

struct Session {
    in_tx: mpsc::UnboundedSender<RawMessage>,
    out_rx: mpsc::UnboundedReceiver<RawMessage>,
    serve: tokio::task::JoinHandle<lspf::Result<lspf::Outcome>>,
}

fn inbound_request(id: i32, method: &'static str, params: Value) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn inbound_response(id: i32, result: Value) -> RawMessage {
    RawMessage::Response {
        id: RequestId::Number(id),
        result: Ok(Bytes::from(serde_json::to_vec(&result).unwrap())),
    }
}

fn cancel_notification(params: Value) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(WorkDoneProgressCancel::METHOD),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn exit() -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed("exit"),
        params: Bytes::from_static(b"null"),
    }
}

impl Session {
    async fn start(server: Server<()>) -> Self {
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let serve = tokio::spawn(server.serve(ChannelTransport {
            incoming: in_rx,
            outgoing: out_tx,
        }));
        in_tx
            .send(inbound_request(
                1,
                "initialize",
                json!({ "processId": null, "rootUri": null, "capabilities": {} }),
            ))
            .unwrap();
        let init_response = Self::recv_from(&mut out_rx).await;
        assert_eq!(init_response.id(), Some(&RequestId::Number(1)));
        Self {
            in_tx,
            out_rx,
            serve,
        }
    }

    async fn recv_from(rx: &mut mpsc::UnboundedReceiver<RawMessage>) -> RawMessage {
        tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("message within 2s")
            .expect("channel open")
    }

    async fn recv(&mut self) -> RawMessage {
        Self::recv_from(&mut self.out_rx).await
    }

    /// The next outbound message must be a request for `method`; answer it
    /// with a `null` success and return its params.
    async fn answer_request(&mut self, method: &str) -> Value {
        match self.recv().await {
            RawMessage::Request {
                id,
                method: m,
                params,
            } => {
                assert_eq!(m.as_ref(), method, "unexpected request method");
                let RequestId::Number(id) = id else {
                    panic!("expected a numeric request id");
                };
                let params: Value = serde_json::from_slice(&params).unwrap();
                self.in_tx.send(inbound_response(id, json!(null))).unwrap();
                params
            }
            other => panic!("expected a {method} request, got {other:?}"),
        }
    }

    /// The next outbound message must be a `$/progress` notification; return
    /// its params.
    async fn expect_progress(&mut self) -> Value {
        match self.recv().await {
            RawMessage::Notification { method, params } => {
                assert_eq!(method.as_ref(), "$/progress", "unexpected notification");
                serde_json::from_slice(&params).unwrap()
            }
            other => panic!("expected a $/progress notification, got {other:?}"),
        }
    }

    /// The next outbound message must be the success response for `id`.
    async fn expect_response(&mut self, id: i32) -> Value {
        match self.recv().await {
            RawMessage::Response {
                id: response_id,
                result: Ok(bytes),
            } => {
                assert_eq!(response_id, RequestId::Number(id), "unexpected response id");
                serde_json::from_slice(&bytes).unwrap()
            }
            other => panic!("expected the response for request {id}, got {other:?}"),
        }
    }

    /// Run a begin lifecycle: the create request is answered, one begin
    /// notification with the verbatim cancellable flag goes out, and the
    /// handler responds with the allocated token.
    async fn begin(&mut self, id: i32, cancellable: bool) -> i32 {
        self.in_tx
            .send(inbound_request(
                id,
                BeginProgress::METHOD,
                json!({ "cancellable": cancellable }),
            ))
            .unwrap();
        let create = self.answer_request("window/workDoneProgress/create").await;
        let token = create["token"].as_i64().expect("numeric token") as i32;
        let begin = self.expect_progress().await;
        assert_eq!(begin["token"], json!(token));
        assert_eq!(begin["value"]["kind"], json!("begin"));
        assert_eq!(begin["value"]["cancellable"], json!(cancellable));
        let response = self.expect_response(id).await;
        assert_eq!(response, json!({ "token": token }));
        token
    }

    /// Probe the stored handle: cancellation state plus the outcome of one
    /// report attempt.
    async fn probe(&mut self, id: i32) -> Value {
        self.in_tx
            .send(inbound_request(id, ProbeProgress::METHOD, json!(null)))
            .unwrap();
        self.expect_response(id).await
    }

    /// One explicit report through the stored handle, observing the exact
    /// work-done report shape.
    async fn report(&mut self, id: i32, token: i32) {
        self.in_tx
            .send(inbound_request(id, ReportProgress::METHOD, json!(null)))
            .unwrap();
        let report = self.expect_progress().await;
        assert_eq!(
            report,
            json!({
                "token": token,
                "value": {
                    "kind": "report",
                    "cancellable": true,
                    "message": "half",
                    "percentage": 50
                }
            })
        );
        let response = self.expect_response(id).await;
        assert_eq!(response, json!({ "report": "ok" }));
    }

    /// End the stored handle's progress with `message`, observing exactly one
    /// work-done end notification.
    async fn end(&mut self, id: i32, token: i32, message: Option<&str>) {
        self.in_tx
            .send(inbound_request(
                id,
                EndProgress::METHOD,
                json!({ "message": message }),
            ))
            .unwrap();
        let end = self.expect_progress().await;
        let mut expected = json!({ "token": token, "value": { "kind": "end" } });
        if let Some(message) = message {
            expected["value"]["message"] = json!(message);
        }
        assert_eq!(end, expected);
        let response = self.expect_response(id).await;
        assert_eq!(response, json!({ "end": "ok" }));
    }

    fn send_cancel(&self, params: Value) {
        self.in_tx.send(cancel_notification(params)).unwrap();
    }

    /// Exit the connection and await its clean termination.
    async fn finish(self) {
        self.in_tx.send(exit()).unwrap();
        self.serve
            .await
            .expect("serve did not panic")
            .expect("serve ended cleanly");
    }
}

// --- Tests -------------------------------------------------------------------

/// Cancellation before any report fires the handle's token and sends nothing
/// by itself: the application's own end is the only end on the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_before_any_report_fires_the_token_without_an_implicit_end() {
    let state = Arc::new(AppState::default());
    let mut session = Session::start(server(state, false)).await;

    let token = session.begin(2, true).await;
    session.send_cancel(json!({ "token": token }));

    // The ordered transport is the assertion: an implicit end enqueued while
    // the cancel was processed would arrive before the probe response.
    let probe = session.probe(3).await;
    assert_eq!(
        probe,
        json!({ "handle": true, "cancelled": true, "report": "cancelled" })
    );

    // The application decides the final message and ends the progress itself.
    session.end(4, token, Some("stopped")).await;
    session.finish().await;
}

/// Cancellation landing after a report fires the token the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_after_a_report_fires_the_token() {
    let state = Arc::new(AppState::default());
    let mut session = Session::start(server(state, false)).await;

    let token = session.begin(2, true).await;
    session.report(3, token).await;
    session.send_cancel(json!({ "token": token }));

    let probe = session.probe(4).await;
    assert_eq!(
        probe,
        json!({ "handle": true, "cancelled": true, "report": "cancelled" })
    );

    session.end(5, token, None).await;
    session.finish().await;
}

/// Unknown and ended tokens are ignored; the connection stays usable for a
/// fresh, still-cancellable lifecycle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_and_ended_tokens_are_ignored_and_the_connection_stays_usable() {
    let state = Arc::new(AppState::default());
    let mut session = Session::start(server(state, false)).await;

    session.send_cancel(json!({ "token": 42 }));

    // The connection still runs a full lifecycle, ending the token.
    let token = session.begin(2, true).await;
    session.end(3, token, None).await;

    // Cancelling the ended token is ignored too.
    session.send_cancel(json!({ "token": token }));

    // A fresh lifecycle proves the connection never noticed: the token
    // sequence kept moving and the new handle still cancels.
    let token = session.begin(4, true).await;
    assert_eq!(token, 2);
    session.send_cancel(json!({ "token": token }));
    let probe = session.probe(5).await;
    assert_eq!(probe["cancelled"], json!(true));
    session.end(6, token, None).await;
    session.finish().await;
}

/// Malformed cancel params are dropped; the hook never runs for them and the
/// connection stays usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_params_are_dropped_without_running_the_hook() {
    let state = Arc::new(AppState::default());
    let mut session = Session::start(server(Arc::clone(&state), true)).await;

    session.send_cancel(json!({ "token": true }));
    session.send_cancel(json!({}));
    session
        .in_tx
        .send(RawMessage::Notification {
            method: Cow::Borrowed(WorkDoneProgressCancel::METHOD),
            params: Bytes::from_static(b"\"not an object\""),
        })
        .unwrap();

    // The connection is unaffected: a full cancellable lifecycle still works
    // and its cancel reaches the hook exactly once.
    let token = session.begin(2, true).await;
    session.send_cancel(json!({ "token": token }));
    let probe = session.probe(3).await;
    assert_eq!(probe["cancelled"], json!(true));
    session.end(4, token, None).await;
    session.finish().await;

    let seen = state.hook_seen.lock().unwrap();
    assert_eq!(
        *seen,
        vec![HookObservation {
            token: json!(token),
            cancelled: Some(true),
        }],
        "only the well-formed cancel ran the hook"
    );
}

/// A non-cancellable token is never fired; reports keep flowing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_cancellable_progress_is_not_cancelled() {
    let state = Arc::new(AppState::default());
    let mut session = Session::start(server(state, false)).await;

    let token = session.begin(2, false).await;
    session.send_cancel(json!({ "token": token }));

    // The probe's report attempt succeeds and emits one report notification.
    session
        .in_tx
        .send(inbound_request(3, ProbeProgress::METHOD, json!(null)))
        .unwrap();
    let report = session.expect_progress().await;
    assert_eq!(report["value"]["kind"], json!("report"));
    assert_eq!(report["value"]["cancellable"], json!(false));
    let probe = session.expect_response(3).await;
    assert_eq!(
        probe,
        json!({ "handle": true, "cancelled": false, "report": "ok" })
    );

    session.end(4, token, None).await;
    session.finish().await;
}

/// The registered hook runs after a successful decode — for an unknown token
/// too — and observes the updated cancellation state on the real cancel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hook_runs_after_decode_and_observes_the_cancelled_state() {
    let state = Arc::new(AppState::default());
    let mut session = Session::start(server(Arc::clone(&state), true)).await;

    let token = session.begin(2, true).await;
    // An unknown token decodes fine, so the hook runs and observes the
    // not-yet-cancelled handle.
    session.send_cancel(json!({ "token": 9 }));
    // The real cancel mutates first; the hook observes the fired token.
    session.send_cancel(json!({ "token": token }));

    // The probe response is the ordering witness: notifications and their
    // hooks are processed on the read-loop before the probe request.
    let probe = session.probe(3).await;
    assert_eq!(probe["cancelled"], json!(true));
    session.end(4, token, None).await;
    session.finish().await;

    let seen = state.hook_seen.lock().unwrap();
    assert_eq!(
        *seen,
        vec![
            HookObservation {
                token: json!(9),
                cancelled: Some(false),
            },
            HookObservation {
                token: json!(token),
                cancelled: Some(true),
            },
        ],
        "the hook observed the state each cancel left behind"
    );
}

/// Session close clears the progress registry; another connection's registry
/// and cancellation are unaffected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_close_clears_the_registry_and_leaves_another_connection_unaffected() {
    let state_a = Arc::new(AppState::default());
    let state_b = Arc::new(AppState::default());
    let mut a = Session::start(server(Arc::clone(&state_a), false)).await;
    let mut b = Session::start(server(Arc::clone(&state_b), false)).await;

    a.begin(2, true).await;
    let token_b = b.begin(2, true).await;

    // Close connection A while its progress is still active.
    a.in_tx.send(exit()).unwrap();
    a.serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");

    // A's registry was cleared: the still-held handle's token is unknown.
    let handle_a = state_a.handle.lock().unwrap().take().expect("begun");
    let error = handle_a.report(None, None).unwrap_err();
    assert!(
        matches!(error, ClientError::Progress(ProgressError::UnknownToken)),
        "a cleared registry reports UnknownToken, got {error:?}"
    );
    let _ = handle_a.end(None);

    // B is untouched: its own token still cancels and ends normally.
    b.send_cancel(json!({ "token": token_b }));
    let probe = b.probe(3).await;
    assert_eq!(
        probe,
        json!({ "handle": true, "cancelled": true, "report": "cancelled" })
    );
    b.end(4, token_b, Some("cancelled by user")).await;
    b.finish().await;
}
