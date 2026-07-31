//! Integration coverage for typed server-to-client requests (issue #46).
//!
//! Handlers issue multiple concurrent typed server-to-client requests and
//! the test delivers responses in reverse order, verifying each caller
//! receives exactly its own correlated result.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use lspf::types::request::Request;
use lspf::{
    ClientError, Context, RawMessage, RequestId, Server, Transport, TransportError,
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

fn inbound_error_response(id: i32, code: i32, message: &'static str) -> RawMessage {
    use lspf::JsonRpcError;
    RawMessage::Response {
        id: RequestId::Number(id),
        result: Err(JsonRpcError {
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
        .send(inbound_error_response(client_req_id, -32001, "test error"))
        .unwrap();

    let trigger_resp = recv(&mut out_rx).await;
    assert_eq!(trigger_resp.id(), Some(&RequestId::Number(2)));

    in_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve did not panic")
        .expect("serve ended cleanly");

    let err = captured_err.lock().unwrap().take().expect("error captured");
    assert!(
        matches!(err, ClientError::Remote { code: -32001, .. }),
        "expected Remote error, got {err:?}"
    );
}

/// Session close completes all pending outbound requests so the server
/// does not hang indefinitely.
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

    let server = Server::builder(())
        .request::<TriggerCloseRequest, _, _>(
            |_state: Arc<()>, ctx: Context, _params: serde_json::Value, _ct| {
                async move {
                    // This will be completed (with an error) by close_all() when
                    // the session closes, so this task will eventually unblock.
                    let _ = ctx
                        .client()
                        .request::<NeverRespondsRequest>(json!({}))
                        .await;
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

    // Consume the outbound client request (never responded to).
    recv(&mut out_rx).await;

    // Close the transport without sending the response.
    // The server must not hang: close_all() will complete the pending request.
    drop(in_tx);

    // The serve future must return within the timeout (not hang forever).
    tokio::time::timeout(std::time::Duration::from_secs(3), serve)
        .await
        .expect("serve returned within timeout — not hanging on pending outbound request")
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
}
