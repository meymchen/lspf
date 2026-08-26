//! Connection-scoped work-done progress lifecycle (issue #109).
//!
//! These tests drive the lifecycle over an in-memory transport: a handler
//! calls `ClientHandle::begin_progress`, the test answers the
//! `window/workDoneProgress/create` request, and the exact wire shape of the
//! `$/progress` begin, report, and end notifications is observed. Registry
//! internals and failure paths that need direct handle access are covered by
//! the unit tests in `src/progress.rs`.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use lspf::types::request::Request;
use lspf::{
    ClientError, ClientHandle, LspError, ProgressHandle, ProgressOptions, RawMessage, RequestId,
    Server, ServerContext, Transport, TransportError, TransportReader, TransportWriter,
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

// --- Helpers -----------------------------------------------------------------

struct Captured {
    client: Mutex<Option<ClientHandle>>,
    handle: Mutex<Option<ProgressHandle>>,
}

enum ProgressDemo {}

impl Request for ProgressDemo {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "test/progress-demo";
}

enum CaptureProgress {}

impl Request for CaptureProgress {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "test/capture-progress";
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

fn inbound_error_response(id: i32, code: i32, message: &str) -> RawMessage {
    RawMessage::Response {
        id: RequestId::Number(id),
        result: Err(lspf::JsonRpcError {
            code,
            message: message.to_string(),
            data: None,
        }),
    }
}

fn exit() -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed("exit"),
        params: Bytes::from_static(b"null"),
    }
}

async fn recv(rx: &mut mpsc::UnboundedReceiver<RawMessage>) -> RawMessage {
    tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("message within 2s")
        .expect("channel open")
}

/// Assert the message is a request for `method` and return its id and params.
fn expect_request(message: &RawMessage, method: &str) -> (i32, Value) {
    match message {
        RawMessage::Request {
            id,
            method: m,
            params,
        } => {
            assert_eq!(m.as_ref(), method, "unexpected request method");
            let RequestId::Number(id) = id else {
                panic!("expected a numeric request id");
            };
            (*id, serde_json::from_slice(params).unwrap())
        }
        other => panic!("expected a {method} request, got {other:?}"),
    }
}

/// Assert the message is a notification for `method` and return its params.
fn expect_notification(message: &RawMessage, method: &str) -> Value {
    match message {
        RawMessage::Notification { method: m, params } => {
            assert_eq!(m.as_ref(), method, "unexpected notification method");
            serde_json::from_slice(params).unwrap()
        }
        other => panic!("expected a {method} notification, got {other:?}"),
    }
}

async fn initialize(
    in_tx: &mpsc::UnboundedSender<RawMessage>,
    out_rx: &mut mpsc::UnboundedReceiver<RawMessage>,
) {
    in_tx
        .send(inbound_request(
            1,
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        ))
        .unwrap();
    let init_resp = recv(out_rx).await;
    assert_eq!(init_resp.id(), Some(&RequestId::Number(1)));
}

// --- Tests -------------------------------------------------------------------

/// The full lifecycle over a real connection: create completes, one begin
/// notification carries the options verbatim, reports use the exact
/// work-done report shape, and end consumes the handle. Two sequential
/// lifecycles prove tokens are monotonic from 1 and independent of the
/// outbound request-ID space.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_lifecycle_over_a_connection() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    let server = Server::builder(())
        .request::<ProgressDemo, _, _>(
            |_state: Arc<()>, ctx: ServerContext, _params: Value, _ct| async move {
                let client = ctx.client();

                let first = client
                    .begin_progress(
                        ProgressOptions::new("Indexing")
                            .cancellable(true)
                            .message("starting")
                            .percentage(0),
                    )
                    .await
                    .map_err(LspError::internal)?;

                // An unrelated outbound request advances the request-ID space
                // without touching the progress-token space.
                client
                    .request::<ProgressDemo>(json!(null))
                    .await
                    .map_err(LspError::internal)?;

                let second = client
                    .begin_progress(ProgressOptions::new("Linking"))
                    .await
                    .map_err(LspError::internal)?;

                let first_token = serde_json::to_value(first.token()).unwrap();
                let second_token = serde_json::to_value(second.token()).unwrap();

                first
                    .report(Some("half".into()), Some(50))
                    .map_err(LspError::internal)?;
                second.report(None, Some(100)).map_err(LspError::internal)?;
                first.end(Some("done".into())).map_err(LspError::internal)?;
                second.end(None).map_err(LspError::internal)?;

                Ok(json!({
                    "first": first_token,
                    "second": second_token,
                }))
            },
        )
        .build()
        .expect("server builds");

    let serve = tokio::spawn(server.serve(ChannelTransport {
        incoming: in_rx,
        outgoing: out_tx,
    }));

    initialize(&in_tx, &mut out_rx).await;

    in_tx
        .send(inbound_request(2, "test/progress-demo", json!(null)))
        .unwrap();

    // First lifecycle: create request with token 1, answered with success.
    let create = recv(&mut out_rx).await;
    let (create_id, params) = expect_request(&create, "window/workDoneProgress/create");
    assert_eq!(params, json!({ "token": 1 }));
    in_tx
        .send(inbound_response(create_id, json!(null)))
        .unwrap();

    // One begin notification with the options verbatim.
    let begin = recv(&mut out_rx).await;
    assert_eq!(
        expect_notification(&begin, "$/progress"),
        json!({
            "token": 1,
            "value": {
                "kind": "begin",
                "title": "Indexing",
                "cancellable": true,
                "message": "starting",
                "percentage": 0
            }
        })
    );

    // The unrelated outbound request gets the next request ID.
    let echo = recv(&mut out_rx).await;
    let (echo_id, _) = expect_request(&echo, "test/progress-demo");
    in_tx.send(inbound_response(echo_id, json!(null))).unwrap();

    // Second lifecycle: the token sequence is independent of the request-ID
    // sequence — token 2 while the request IDs have moved on.
    let create = recv(&mut out_rx).await;
    let (create_id, params) = expect_request(&create, "window/workDoneProgress/create");
    assert_eq!(params, json!({ "token": 2 }));
    assert!(create_id > echo_id, "request IDs keep their own sequence");
    in_tx
        .send(inbound_response(create_id, json!(null)))
        .unwrap();

    let begin = recv(&mut out_rx).await;
    assert_eq!(
        expect_notification(&begin, "$/progress"),
        json!({
            "token": 2,
            "value": { "kind": "begin", "title": "Linking", "cancellable": false }
        })
    );

    // Reports use the exact work-done report shape; the second handle's
    // report omits unset fields and keeps its own cancellable flag.
    let report = recv(&mut out_rx).await;
    assert_eq!(
        expect_notification(&report, "$/progress"),
        json!({
            "token": 1,
            "value": { "kind": "report", "cancellable": true, "message": "half", "percentage": 50 }
        })
    );
    let report = recv(&mut out_rx).await;
    assert_eq!(
        expect_notification(&report, "$/progress"),
        json!({ "token": 2, "value": { "kind": "report", "cancellable": false, "percentage": 100 } })
    );

    // Ends carry the optional message and nothing else.
    let end = recv(&mut out_rx).await;
    assert_eq!(
        expect_notification(&end, "$/progress"),
        json!({ "token": 1, "value": { "kind": "end", "message": "done" } })
    );
    let end = recv(&mut out_rx).await;
    assert_eq!(
        expect_notification(&end, "$/progress"),
        json!({ "token": 2, "value": { "kind": "end" } })
    );

    // The handler's response reports both tokens.
    let response = recv(&mut out_rx).await;
    assert_eq!(response.id(), Some(&RequestId::Number(2)));
    match response {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => {
            let result: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(result, json!({ "first": 1, "second": 2 }));
        }
        other => panic!("expected a success response, got {other:?}"),
    }

    in_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");
}

/// One full lifecycle pinned against the deterministic wire fixtures: the
/// exact JSON of the `window/workDoneProgress/create` request and the
/// begin, report, and end `$/progress` notifications. The fixtures exist so
/// a wire-shape change is a deliberate, reviewed edit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_lifecycle_matches_the_wire_fixtures() {
    const CREATE_FIXTURE: &str = include_str!("fixtures/work_done_progress_create_request.json");
    const BEGIN_FIXTURE: &str = include_str!("fixtures/progress_begin_notification.json");
    const REPORT_FIXTURE: &str = include_str!("fixtures/progress_report_notification.json");
    const END_FIXTURE: &str = include_str!("fixtures/progress_end_notification.json");

    /// Assert the wire shape — method plus decoded params — matches the
    /// fixture, for requests and notifications alike.
    fn assert_wire_fixture(message: &RawMessage, fixture: &str) {
        let (method, params) = match message {
            RawMessage::Request { method, params, .. }
            | RawMessage::Notification { method, params } => (method, params),
            other => panic!("expected a request or notification, got {other:?}"),
        };
        let wire = json!({
            "method": method.as_ref(),
            "params": serde_json::from_slice::<Value>(params)
                .expect("the params are valid JSON"),
        });
        let expected: Value = serde_json::from_str(fixture).expect("the fixture is valid JSON");
        assert_eq!(wire, expected, "the wire shape must match the fixture");
    }

    enum FixtureRun {}

    impl Request for FixtureRun {
        type Params = Value;
        type Result = Value;
        const METHOD: &'static str = "test/progress-fixture";
    }

    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    let server = Server::builder(())
        .request::<FixtureRun, _, _>(
            |_state: Arc<()>, ctx: ServerContext, _params: Value, _ct| async move {
                let handle = ctx
                    .client()
                    .begin_progress(
                        ProgressOptions::new("Indexing")
                            .cancellable(true)
                            .message("starting")
                            .percentage(0),
                    )
                    .await
                    .map_err(LspError::internal)?;
                handle
                    .report(Some("half".into()), Some(50))
                    .map_err(LspError::internal)?;
                handle
                    .end(Some("done".into()))
                    .map_err(LspError::internal)?;
                Ok(json!(null))
            },
        )
        .build()
        .expect("server builds");

    let serve = tokio::spawn(server.serve(ChannelTransport {
        incoming: in_rx,
        outgoing: out_tx,
    }));

    initialize(&in_tx, &mut out_rx).await;
    in_tx
        .send(inbound_request(2, "test/progress-fixture", json!(null)))
        .unwrap();

    // A fresh connection's first progress token is deterministically 1, which
    // is what the fixtures pin.
    let create = recv(&mut out_rx).await;
    assert_wire_fixture(&create, CREATE_FIXTURE);
    let (create_id, _) = expect_request(&create, "window/workDoneProgress/create");
    in_tx
        .send(inbound_response(create_id, json!(null)))
        .unwrap();

    let begin = recv(&mut out_rx).await;
    assert_wire_fixture(&begin, BEGIN_FIXTURE);
    let report = recv(&mut out_rx).await;
    assert_wire_fixture(&report, REPORT_FIXTURE);
    let end = recv(&mut out_rx).await;
    assert_wire_fixture(&end, END_FIXTURE);

    let response = recv(&mut out_rx).await;
    assert_eq!(response.id(), Some(&RequestId::Number(2)));

    in_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");
}

/// A remote refusal of `window/workDoneProgress/create` surfaces as
/// `ClientError::Remote` and no begin notification is ever sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_remote_failure_sends_no_begin_over_a_connection() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    enum BeginOnce {}

    impl Request for BeginOnce {
        type Params = Value;
        type Result = Value;
        const METHOD: &'static str = "test/begin-once";
    }

    let server = Server::builder(())
        .request::<BeginOnce, _, _>(
            |_state: Arc<()>, ctx: ServerContext, _params: Value, _ct| async move {
                let error = ctx
                    .client()
                    .begin_progress(ProgressOptions::new("Indexing"))
                    .await
                    .unwrap_err();
                let kind = match error {
                    ClientError::Remote(remote) => format!("remote:{}", remote.code),
                    other => panic!("expected ClientError::Remote, got {other:?}"),
                };
                Ok(json!({ "error": kind }))
            },
        )
        .build()
        .expect("server builds");

    let serve = tokio::spawn(server.serve(ChannelTransport {
        incoming: in_rx,
        outgoing: out_tx,
    }));

    initialize(&in_tx, &mut out_rx).await;
    in_tx
        .send(inbound_request(2, "test/begin-once", json!(null)))
        .unwrap();

    let create = recv(&mut out_rx).await;
    let (create_id, _) = expect_request(&create, "window/workDoneProgress/create");
    in_tx
        .send(inbound_error_response(create_id, -32803, "Request failed"))
        .unwrap();

    // The very next outbound message is the handler's response: the failed
    // lifecycle emitted no begin notification in between.
    let response = recv(&mut out_rx).await;
    assert_eq!(response.id(), Some(&RequestId::Number(2)));
    match response {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => {
            let result: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(result, json!({ "error": "remote:-32803" }));
        }
        other => panic!("expected a success response, got {other:?}"),
    }

    in_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");
}

/// After the connection closes, a still-held handle and `ClientHandle` fail fast:
/// no report, end, or new begin can be sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_close_fails_every_further_progress_operation() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    let captured: Arc<Captured> = Arc::new(Captured {
        client: Mutex::new(None),
        handle: Mutex::new(None),
    });

    let state = Arc::clone(&captured);
    let server = Server::builder(())
        .request::<CaptureProgress, _, _>(
            move |_state: Arc<()>, ctx: ServerContext, _params: Value, _ct| {
                let state = Arc::clone(&state);
                async move {
                    let client = ctx.client();
                    let handle = client
                        .begin_progress(ProgressOptions::new("Indexing"))
                        .await
                        .map_err(LspError::internal)?;
                    *state.client.lock().unwrap() = Some(client);
                    *state.handle.lock().unwrap() = Some(handle);
                    Ok(json!(null))
                }
            },
        )
        .build()
        .expect("server builds");

    let serve = tokio::spawn(server.serve(ChannelTransport {
        incoming: in_rx,
        outgoing: out_tx,
    }));

    initialize(&in_tx, &mut out_rx).await;
    in_tx
        .send(inbound_request(2, "test/capture-progress", json!(null)))
        .unwrap();

    let create = recv(&mut out_rx).await;
    let (create_id, _) = expect_request(&create, "window/workDoneProgress/create");
    in_tx
        .send(inbound_response(create_id, json!(null)))
        .unwrap();
    let begin = recv(&mut out_rx).await;
    expect_notification(&begin, "$/progress");
    let response = recv(&mut out_rx).await;
    assert_eq!(response.id(), Some(&RequestId::Number(2)));

    in_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");

    let client = captured.client.lock().unwrap().clone().expect("captured");
    let handle = captured.handle.lock().unwrap().take().expect("captured");

    // A new begin fails before allocating or sending anything.
    let error = client
        .begin_progress(ProgressOptions::new("Late"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            ClientError::ConnectionClosed | ClientError::OutboundClosed
        ),
        "begin after close fails fast, got {error:?}"
    );

    // Session close cleared the progress registry: the still-held handle's
    // token is unknown before any I/O is even attempted.
    let error = handle.report(None, Some(10)).unwrap_err();
    assert!(
        matches!(
            error,
            ClientError::Progress(lspf::ProgressError::UnknownToken)
        ),
        "report after close sees the cleared registry, got {error:?}"
    );

    // End still removes the token even though its enqueue fails.
    let error = handle.end(None).unwrap_err();
    assert!(
        matches!(
            error,
            ClientError::ConnectionClosed | ClientError::OutboundClosed
        ),
        "end after close fails fast, got {error:?}"
    );

    assert!(out_rx.try_recv().is_err(), "nothing more was sent");
}
