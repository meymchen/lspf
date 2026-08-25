//! Integration coverage for typed server-to-client requests (issue #46).
//!
//! Handlers issue multiple concurrent typed server-to-client requests and
//! the test delivers responses in reverse order, verifying each caller
//! receives exactly its own correlated result.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use lspf::types::request::Request;
use lspf::{
    ClientError, Context, RawMessage, RequestId, ResourcePolicy, Server, Transport, TransportError,
    TransportReader, TransportWriter,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

// --- Custom marker types -----------------------------------------------------

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct EchoResult {
    echoed: u32,
}

enum EchoRequest {}

impl Request for EchoRequest {
    type Params = serde_json::Value;
    type Result = EchoResult;
    const METHOD: &'static str = "client/echo";
}

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

fn inbound_request(id: i32, method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn inbound_response(id: i32, result: serde_json::Value) -> RawMessage {
    RawMessage::Response {
        id: RequestId::Number(id),
        result: Ok(Bytes::from(serde_json::to_vec(&result).unwrap())),
    }
}

fn inbound_error_response(
    id: i32,
    code: i32,
    message: &'static str,
    data: Option<serde_json::Value>,
) -> RawMessage {
    use lspf::JsonRpcError;
    RawMessage::Response {
        id: RequestId::Number(id),
        result: Err(JsonRpcError {
            code,
            message: message.to_string(),
            data,
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
    recv_within(rx, Duration::from_secs(2)).await
}

async fn recv_within(
    rx: &mut mpsc::UnboundedReceiver<RawMessage>,
    timeout: Duration,
) -> RawMessage {
    tokio::time::timeout(timeout, rx.recv())
        .await
        .expect("message within watchdog")
        .expect("channel open")
}

// --- Tests -------------------------------------------------------------------

/// Two concurrent server-to-client requests receive their responses in reverse
/// order and each completes with the correct correlated result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_client_requests_complete_in_reverse_order() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    let results: Arc<Mutex<Vec<Result<EchoResult, ClientError>>>> =
        Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&results);

    enum TriggerRequest {}
    impl Request for TriggerRequest {
        type Params = serde_json::Value;
        type Result = serde_json::Value;
        const METHOD: &'static str = "test/trigger";
    }

    let server = Server::builder(())
        .request::<TriggerRequest, _, _>(
            move |_state: Arc<()>, ctx: Context, _params: serde_json::Value, _ct| {
                let captured = Arc::clone(&captured);
                async move {
                    let client = ctx.client();
                    let c1 = client.clone();
                    let c2 = client.clone();

                    let (r1, r2) = tokio::join!(
                        c1.request::<EchoRequest>(json!(null)),
                        c2.request::<EchoRequest>(json!(null)),
                    );

                    captured.lock().unwrap().push(r1);
                    captured.lock().unwrap().push(r2);
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

    // Initialize.
    in_tx
        .send(inbound_request(
            1,
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        ))
        .unwrap();
    let init_resp = recv(&mut out_rx).await;
    assert_eq!(init_resp.id(), Some(&RequestId::Number(1)));

    // Trigger the handler which blocks on two concurrent outbound requests.
    in_tx
        .send(inbound_request(2, "test/trigger", json!(null)))
        .unwrap();

    // Collect the two outbound client-request messages.
    let msg_a = recv(&mut out_rx).await;
    let msg_b = recv(&mut out_rx).await;

    let id_a = match msg_a.id() {
        Some(RequestId::Number(n)) => *n,
        _ => panic!("expected numeric request id for msg_a"),
    };
    let id_b = match msg_b.id() {
        Some(RequestId::Number(n)) => *n,
        _ => panic!("expected numeric request id for msg_b"),
    };

    // IDs must be positive and distinct.
    assert!(id_a >= 1, "outbound ID must be positive");
    assert!(id_b >= 1, "outbound ID must be positive");
    assert_ne!(id_a, id_b, "concurrent requests must have distinct IDs");

    // Deliver responses in reverse order (b first).
    in_tx
        .send(inbound_response(id_b, json!({ "echoed": 99 })))
        .unwrap();
    in_tx
        .send(inbound_response(id_a, json!({ "echoed": 42 })))
        .unwrap();

    // The handler completes and the trigger response arrives.
    let trigger_resp = recv(&mut out_rx).await;
    assert_eq!(trigger_resp.id(), Some(&RequestId::Number(2)));

    in_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");

    let captured = results.lock().unwrap();
    assert_eq!(captured.len(), 2, "handler captured two results");
    for r in captured.iter() {
        assert!(r.is_ok(), "expected Ok result, got {r:?}");
    }
    let mut echoed: Vec<u32> = captured
        .iter()
        .map(|r| r.as_ref().unwrap().echoed)
        .collect();
    echoed.sort_unstable();
    assert_eq!(
        echoed,
        vec![42, 99],
        "each request received its own response"
    );
}

/// Unknown response IDs are ignored and the connection continues normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_response_id_does_not_terminate_connection() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    enum NoopRequest {}
    impl Request for NoopRequest {
        type Params = serde_json::Value;
        type Result = serde_json::Value;
        const METHOD: &'static str = "test/noop";
    }

    let server = Server::builder(())
        .request::<NoopRequest, _, _>(
            |_state: Arc<()>, _ctx: Context, _params: serde_json::Value, _ct| async {
                Ok(json!(null))
            },
        )
        .build()
        .expect("server builds");

    let serve = tokio::spawn(server.serve(ChannelTransport {
        incoming: in_rx,
        outgoing: out_tx,
    }));

    // Initialize.
    in_tx
        .send(inbound_request(
            1,
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        ))
        .unwrap();
    recv(&mut out_rx).await;

    // Send a response with an ID the server never allocated.
    in_tx
        .send(inbound_response(9999, json!("ignored")))
        .unwrap();

    // The connection must still handle a normal request after the rogue response.
    in_tx
        .send(inbound_request(2, "test/noop", json!(null)))
        .unwrap();
    let noop_resp = recv(&mut out_rx).await;
    assert_eq!(noop_resp.id(), Some(&RequestId::Number(2)));

    in_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");
}

/// A remote JSON-RPC error response becomes `ClientError::Remote`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_error_response_becomes_client_error_remote() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    let captured_err: Arc<Mutex<Option<ClientError>>> = Arc::new(Mutex::new(None));

    enum TriggerErrRequest {}
    impl Request for TriggerErrRequest {
        type Params = serde_json::Value;
        type Result = serde_json::Value;
        const METHOD: &'static str = "test/trigger-err";
    }

    enum EchoClientRequest {}
    impl Request for EchoClientRequest {
        type Params = serde_json::Value;
        type Result = serde_json::Value;
        const METHOD: &'static str = "client/echo-err";
    }

    let captured = Arc::clone(&captured_err);
    let server = Server::builder(())
        .request::<TriggerErrRequest, _, _>(
            move |_state: Arc<()>, ctx: Context, _params: serde_json::Value, _ct| {
                let captured = Arc::clone(&captured);
                async move {
                    let err = ctx
                        .client()
                        .request::<EchoClientRequest>(json!({}))
                        .await
                        .unwrap_err();
                    *captured.lock().unwrap() = Some(err);
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

    in_tx
        .send(inbound_request(
            1,
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        ))
        .unwrap();
    recv(&mut out_rx).await;

    in_tx
        .send(inbound_request(2, "test/trigger-err", json!(null)))
        .unwrap();

    let outbound = recv(&mut out_rx).await;
    let client_req_id = match outbound.id() {
        Some(RequestId::Number(n)) => *n,
        _ => panic!("expected numeric id"),
    };

    in_tx
        .send(inbound_error_response(
            client_req_id,
            -32001,
            "test error",
            Some(json!({ "detail": "transient" })),
        ))
        .unwrap();

    let trigger_resp = recv(&mut out_rx).await;
    assert_eq!(trigger_resp.id(), Some(&RequestId::Number(2)));

    in_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");

    // The remote error's code, message, and optional data are preserved.
    let err = captured_err.lock().unwrap().take().expect("error captured");
    match err {
        ClientError::Remote(e) => {
            assert_eq!(e.code, -32001);
            assert_eq!(e.message, "test error");
            assert_eq!(e.data, Some(json!({ "detail": "transient" })));
        }
        other => panic!("expected Remote error, got {other:?}"),
    }
}

/// Session close completes all pending outbound requests with
/// `ClientError::Cancelled` so the server does not hang indefinitely.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_close_does_not_hang_with_pending_client_request() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    enum TriggerCloseRequest {}
    impl Request for TriggerCloseRequest {
        type Params = serde_json::Value;
        type Result = serde_json::Value;
        const METHOD: &'static str = "test/trigger-close";
    }

    enum NeverRespondsRequest {}
    impl Request for NeverRespondsRequest {
        type Params = serde_json::Value;
        type Result = serde_json::Value;
        const METHOD: &'static str = "client/never-responds";
    }

    // The handler records the outcome on a detached task (not tracked by the
    // engine's task group), so it survives `abort_and_join()` when the session
    // closes and observes the pending request complete with `Cancelled`.
    let outcome: Arc<Mutex<Option<Result<serde_json::Value, ClientError>>>> =
        Arc::new(Mutex::new(None));
    let captured = Arc::clone(&outcome);

    let server = Server::builder(())
        .request::<TriggerCloseRequest, _, _>(
            move |_state: Arc<()>, ctx: Context, _params: serde_json::Value, _ct| {
                let captured = Arc::clone(&captured);
                async move {
                    let client = ctx.client();
                    tokio::spawn(async move {
                        let result = client.request::<NeverRespondsRequest>(json!({})).await;
                        *captured.lock().unwrap() = Some(result);
                    });
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

    in_tx
        .send(inbound_request(
            1,
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        ))
        .unwrap();
    recv(&mut out_rx).await;

    in_tx
        .send(inbound_request(2, "test/trigger-close", json!(null)))
        .unwrap();

    // Consume outbound messages until the client request appears. The trigger
    // response may or may not precede it, so skip anything else.
    loop {
        match recv(&mut out_rx).await {
            RawMessage::Request {
                id: RequestId::Number(_),
                method,
                ..
            } if &*method == "client/never-responds" => break,
            _ => {}
        }
    }

    // Close the transport without sending the response.
    // The server must not hang: close_all() completes the pending request.
    drop(in_tx);

    // The serve future must return within the timeout (not hang forever).
    tokio::time::timeout(std::time::Duration::from_secs(3), serve)
        .await
        .expect("serve returned within timeout — not hanging on pending outbound request")
        .expect("serve task did not panic")
        .expect("serve ended cleanly");

    // The detached task observes the pending request complete with Cancelled.
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(result) = outcome.lock().unwrap().take() {
                return result;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("cancellation outcome recorded");
    assert!(
        matches!(result, Err(ClientError::Cancelled)),
        "expected ClientError::Cancelled, got {result:?}"
    );
}

/// Abandoning an enqueued client request emits one typed `$/cancelRequest`
/// notification carrying the abandoned request's ID.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoned_client_request_sends_cancel_notification() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    enum AbandonRequest {}
    impl Request for AbandonRequest {
        type Params = serde_json::Value;
        type Result = serde_json::Value;
        const METHOD: &'static str = "test/abandon";
    }

    let server = Server::builder(())
        .request::<AbandonRequest, _, _>(
            |_state: Arc<()>, ctx: Context, _params: serde_json::Value, _ct| {
                async move {
                    let client = ctx.client();
                    // Enqueue a client request, then abandon it before any
                    // response arrives. Dropping the future must emit a typed
                    // `$/cancelRequest` for the request's ID.
                    let fut = client.request::<EchoRequest>(json!(null));
                    tokio::select! {
                        _ = fut => {}
                        _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
                    }
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

    in_tx
        .send(inbound_request(
            1,
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        ))
        .unwrap();
    recv(&mut out_rx).await;

    in_tx
        .send(inbound_request(2, "test/abandon", json!(null)))
        .unwrap();

    // The outbound client request, then the cancellation notification.
    let req = recv(&mut out_rx).await;
    let req_id = match req.id() {
        Some(RequestId::Number(n)) => *n,
        _ => panic!("expected numeric client request id"),
    };

    let cancel = recv(&mut out_rx).await;
    match cancel {
        RawMessage::Notification { method, params } => {
            assert_eq!(&*method, "$/cancelRequest");
            let params: serde_json::Value = serde_json::from_slice(&params).unwrap();
            assert_eq!(params["id"], serde_json::json!(req_id));
        }
        _ => panic!("expected a $/cancelRequest notification"),
    }

    // The handler returns Ok; the trigger response follows.
    let trigger_resp = recv(&mut out_rx).await;
    assert_eq!(trigger_resp.id(), Some(&RequestId::Number(2)));

    in_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");
}

/// A stale response for an abandoned request cannot complete a later request:
/// outbound IDs are never reused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_response_after_cleanup_cannot_complete_another_request() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    let captured: Arc<Mutex<Option<Result<EchoResult, ClientError>>>> = Arc::new(Mutex::new(None));
    let captured_for_handler = Arc::clone(&captured);

    enum StaleRequest {}
    impl Request for StaleRequest {
        type Params = serde_json::Value;
        type Result = serde_json::Value;
        const METHOD: &'static str = "test/stale";
    }

    let server = Server::builder(())
        .request::<StaleRequest, _, _>(
            move |_state: Arc<()>, ctx: Context, _params: serde_json::Value, _ct| {
                let captured = Arc::clone(&captured_for_handler);
                async move {
                    let client = ctx.client();
                    // Request A is enqueued and then abandoned.
                    let fut_a = client.request::<EchoRequest>(json!(null));
                    tokio::select! {
                        _ = fut_a => {}
                        _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
                    }
                    // Request B follows; it must not reuse A's ID.
                    let result_b = client.request::<EchoRequest>(json!(null)).await;
                    *captured.lock().unwrap() = Some(result_b);
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

    in_tx
        .send(inbound_request(
            1,
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        ))
        .unwrap();
    recv(&mut out_rx).await;

    in_tx
        .send(inbound_request(2, "test/stale", json!(null)))
        .unwrap();

    // Wire order: request A, its cancellation, then request B.
    let msg_a = recv(&mut out_rx).await;
    let id_a = match msg_a.id() {
        Some(RequestId::Number(n)) => *n,
        _ => panic!("expected numeric id for request A"),
    };
    let cancel = recv(&mut out_rx).await;
    match cancel {
        RawMessage::Notification { method, params } => {
            assert_eq!(&*method, "$/cancelRequest");
            let params: serde_json::Value = serde_json::from_slice(&params).unwrap();
            assert_eq!(params["id"], serde_json::json!(id_a));
        }
        _ => panic!("expected a $/cancelRequest notification"),
    }
    let msg_b = recv(&mut out_rx).await;
    let id_b = match msg_b.id() {
        Some(RequestId::Number(n)) => *n,
        _ => panic!("expected numeric id for request B"),
    };
    assert_ne!(id_a, id_b, "abandoned request's ID must never be reused");

    // Deliver a stale response for the abandoned request A. Its entry is gone,
    // so this must be ignored and cannot complete request B.
    in_tx
        .send(inbound_response(id_a, json!({ "echoed": 999 })))
        .unwrap();

    // Then deliver the real response for request B.
    in_tx
        .send(inbound_response(id_b, json!({ "echoed": 42 })))
        .unwrap();

    // The handler completes with B's own result.
    let trigger_resp = recv(&mut out_rx).await;
    assert_eq!(trigger_resp.id(), Some(&RequestId::Number(2)));

    in_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");

    let result_b = captured.lock().unwrap().take().expect("handler captured B");
    assert_eq!(result_b.unwrap(), EchoResult { echoed: 42 });
}

/// An expired request is removed and cancelled once; its late response cannot
/// complete the next request, whose monotonically allocated ID is distinct.
#[tokio::test(start_paused = true)]
async fn expired_request_is_cancelled_and_cannot_capture_a_later_response() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    type CapturedResults = (
        Result<EchoResult, ClientError>,
        Result<EchoResult, ClientError>,
    );
    let captured: Arc<Mutex<Option<CapturedResults>>> = Arc::new(Mutex::new(None));
    let captured_for_handler = Arc::clone(&captured);

    enum TimeoutRequest {}
    impl Request for TimeoutRequest {
        type Params = serde_json::Value;
        type Result = serde_json::Value;
        const METHOD: &'static str = "test/timeout";
    }

    let server = Server::builder(())
        .request::<TimeoutRequest, _, _>(
            move |_state: Arc<()>, ctx: Context, _params: serde_json::Value, _ct| {
                let captured = Arc::clone(&captured_for_handler);
                async move {
                    let client = ctx.client();
                    let first = client.request::<EchoRequest>(json!(null)).await;
                    let second = client.request::<EchoRequest>(json!(null)).await;
                    *captured.lock().unwrap() = Some((first, second));
                    Ok(json!(null))
                }
            },
        )
        .resource_policy(ResourcePolicy {
            handler_timeout: Duration::from_secs(120),
            ..ResourcePolicy::default()
        })
        .build()
        .expect("server builds");

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
    recv(&mut out_rx).await;

    in_tx
        .send(inbound_request(2, TimeoutRequest::METHOD, json!(null)))
        .unwrap();

    let first_request = recv(&mut out_rx).await;
    let first_id = match first_request.id() {
        Some(RequestId::Number(id)) => *id,
        _ => panic!("expected the first outbound request"),
    };

    let cancellation = recv_within(&mut out_rx, Duration::from_secs(31)).await;
    match cancellation {
        RawMessage::Notification { method, params } => {
            assert_eq!(&*method, "$/cancelRequest");
            let params: serde_json::Value = serde_json::from_slice(&params).unwrap();
            assert_eq!(params["id"], json!(first_id));
        }
        other => panic!("expected one cancellation after expiry, got {other:?}"),
    }

    let second_request = recv(&mut out_rx).await;
    let second_id = match second_request.id() {
        Some(RequestId::Number(id)) => *id,
        _ => panic!("expected the second outbound request"),
    };
    assert!(second_id > first_id, "expired IDs are never reused");

    in_tx
        .send(inbound_response(first_id, json!({ "echoed": 999 })))
        .unwrap();
    assert!(
        out_rx.try_recv().is_err(),
        "the late response cannot complete the second request"
    );

    in_tx
        .send(inbound_response(second_id, json!({ "echoed": 42 })))
        .unwrap();
    let trigger_response = recv(&mut out_rx).await;
    assert_eq!(trigger_response.id(), Some(&RequestId::Number(2)));
    assert!(
        out_rx.try_recv().is_err(),
        "a completed request emits no cancellation"
    );

    in_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");

    let (first, second) = captured.lock().unwrap().take().expect("results captured");
    assert!(matches!(first, Err(ClientError::Timeout)));
    assert_eq!(second.unwrap(), EchoResult { echoed: 42 });
}

/// Explicitly disabling the outbound deadline leaves the request pending past
/// the finite default until its correlated response arrives.
#[tokio::test(start_paused = true)]
async fn explicitly_disabled_deadline_waits_for_the_peer_response() {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    let captured: Arc<Mutex<Option<Result<EchoResult, ClientError>>>> = Arc::new(Mutex::new(None));
    let captured_for_handler = Arc::clone(&captured);

    enum DisabledTimeoutRequest {}
    impl Request for DisabledTimeoutRequest {
        type Params = serde_json::Value;
        type Result = serde_json::Value;
        const METHOD: &'static str = "test/disabled-timeout";
    }

    let server = Server::builder(())
        .request::<DisabledTimeoutRequest, _, _>(
            move |_state: Arc<()>, ctx: Context, _params: serde_json::Value, _ct| {
                let captured = Arc::clone(&captured_for_handler);
                async move {
                    let result = ctx.client().request::<EchoRequest>(json!(null)).await;
                    *captured.lock().unwrap() = Some(result);
                    Ok(json!(null))
                }
            },
        )
        .resource_policy(ResourcePolicy {
            outbound_request_timeout: None,
            handler_timeout: Duration::from_secs(120),
            ..ResourcePolicy::default()
        })
        .build()
        .expect("server builds");

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
    recv(&mut out_rx).await;

    in_tx
        .send(inbound_request(
            2,
            DisabledTimeoutRequest::METHOD,
            json!(null),
        ))
        .unwrap();
    let outbound = recv(&mut out_rx).await;
    let request_id = match outbound.id() {
        Some(RequestId::Number(id)) => *id,
        _ => panic!("expected an outbound request"),
    };

    assert!(
        tokio::time::timeout(Duration::from_secs(31), out_rx.recv())
            .await
            .is_err(),
        "no timeout or cancellation is emitted after the default boundary"
    );

    in_tx
        .send(inbound_response(request_id, json!({ "echoed": 42 })))
        .unwrap();
    let trigger_response = recv(&mut out_rx).await;
    assert_eq!(trigger_response.id(), Some(&RequestId::Number(2)));

    in_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");

    let result = captured.lock().unwrap().take().expect("result captured");
    assert_eq!(result.unwrap(), EchoResult { echoed: 42 });
}
