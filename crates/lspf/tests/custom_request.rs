//! End-to-end coverage for the 0.2 typed custom request slice (issue #39).
//!
//! A connection-owned [`Server`] built from application state registers one
//! typed custom request through a marker implementing the re-exported
//! `Request` trait, and serves it over an in-memory channel-backed
//! [`Transport`] alongside the `initialize` / `shutdown` lifecycle. The tests
//! drive real envelopes through that transport and inspect the outbox.

mod common;

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

use lspf::types::request::Request;
use lspf::{
    Context, RawMessage, RequestId, Server, Transport, TransportError, TransportReader,
    TransportWriter,
};

// --- Custom request marker ---------------------------------------------------

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct GreetParams {
    name: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct GreetResult {
    greeting: String,
}

/// The marker fixes the wire method and the parameter/result types dispatch
/// uses. It implements lspf's re-export of `lsp_types::request::Request`.
enum Greet {}

impl Request for Greet {
    type Params = GreetParams;
    type Result = GreetResult;
    const METHOD: &'static str = "custom/greet";
}

/// Application state shared as `Arc<S>` by every handler on the connection.
struct AppState {
    /// How many times the greet handler actually ran — proves malformed
    /// params never reach it. An `Arc` so the test can observe it after the
    /// state has moved into the server.
    handled: Arc<AtomicUsize>,
    /// Prefix mixed into each greeting, proving the handler reads state.
    prefix: String,
}

async fn greet(
    state: Arc<AppState>,
    _ctx: Context,
    params: GreetParams,
    _ct: lspf::CancellationToken,
) -> Result<GreetResult, lspf::LspError> {
    state.handled.fetch_add(1, Ordering::SeqCst);
    Ok(GreetResult {
        greeting: format!("{}, {}!", state.prefix, params.name),
    })
}

// --- In-memory transport -----------------------------------------------------

struct ChannelTransport {
    in_rx: mpsc::UnboundedReceiver<RawMessage>,
    out_tx: mpsc::UnboundedSender<RawMessage>,
}

struct ChannelReader {
    in_rx: mpsc::UnboundedReceiver<RawMessage>,
}

struct ChannelWriter {
    out_tx: mpsc::UnboundedSender<RawMessage>,
}

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            ChannelReader { in_rx: self.in_rx },
            ChannelWriter {
                out_tx: self.out_tx,
            },
        )
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.in_rx.recv().await.ok_or(TransportError::Closed)
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, msg: RawMessage) -> Result<(), TransportError> {
        self.out_tx.send(msg).map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

// --- Envelope helpers --------------------------------------------------------

fn request(id: i32, method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn initialize_request(id: i32) -> RawMessage {
    request(
        id,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    )
}

fn notification(method: &'static str) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from_static(b"null"),
    }
}

/// Build the greet server, feed it `messages`, then close the transport so
/// `serve` returns once everything is processed. Returns the outbox and a
/// count of how many times the greet handler ran.
async fn drive(messages: Vec<RawMessage>) -> (Vec<RawMessage>, usize) {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let transport = ChannelTransport { in_rx, out_tx };

    let handled = Arc::new(AtomicUsize::new(0));
    let server = Server::builder(AppState {
        handled: Arc::clone(&handled),
        prefix: "Hello".to_string(),
    })
    .request::<Greet, _, _>(greet)
    .build()
    .expect("server builds");

    let handle = tokio::spawn(async move { server.serve(transport).await });

    let mut outbox = Vec::new();
    for msg in messages {
        let response_id = msg.id().cloned();
        in_tx.send(msg).unwrap();
        if let Some(response_id) = response_id {
            let response = tokio::time::timeout(Duration::from_secs(2), out_rx.recv())
                .await
                .expect("response arrived within 2s")
                .expect("writer remained open");
            assert_eq!(response.id(), Some(&response_id));
            outbox.push(response);
        }
    }
    drop(in_tx); // peer disconnect → serve drains and returns

    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("serve returned within 2s")
        .expect("server task did not panic")
        .expect("serve ended cleanly");

    outbox.extend(std::iter::from_fn(|| out_rx.try_recv().ok()));
    (outbox, handled.load(Ordering::SeqCst))
}

fn response(outbox: &[RawMessage], id: i32) -> Option<&RawMessage> {
    outbox.iter().find(
        |m| matches!(m, RawMessage::Response { id: rid, .. } if *rid == RequestId::Number(id)),
    )
}

fn ok_result(outbox: &[RawMessage], id: i32) -> Option<serde_json::Value> {
    match response(outbox, id)? {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => Some(serde_json::from_slice(bytes).unwrap()),
        _ => None,
    }
}

fn error_code(outbox: &[RawMessage], id: i32) -> Option<i32> {
    match response(outbox, id)? {
        RawMessage::Response { result: Err(e), .. } => Some(e.code),
        _ => None,
    }
}

// --- Tests -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_custom_request_shutdown_round_trip() {
    let (outbox, handled) = drive(vec![
        initialize_request(1),
        request(2, "custom/greet", json!({ "name": "Ada" })),
        request(3, "shutdown", json!(null)),
        notification("exit"),
    ])
    .await;

    // initialize succeeded and advertised no custom capabilities. Only the
    // protocol-owned fields remain: the negotiated position encoding
    // (ADR 0016) — the client offered none, so it defaults to UTF-16 — and the
    // document sync and workspace-folder support the engine's built-ins perform.
    let init: lspf::types::InitializeResult =
        serde_json::from_value(ok_result(&outbox, 1).expect("initialize response")).unwrap();
    assert_eq!(
        init.capabilities,
        lspf::types::ServerCapabilities {
            position_encoding: Some(lspf::types::PositionEncodingKind::UTF16),
            text_document_sync: Some(lspf::types::TextDocumentSyncCapability::Kind(
                lspf::types::TextDocumentSyncKind::INCREMENTAL,
            )),
            workspace: Some(common::workspace_capabilities()),
            ..lspf::types::ServerCapabilities::default()
        },
        "custom requests must not add ServerCapabilities beyond protocol-owned fields"
    );

    // The custom request decoded, ran its typed handler, and encoded a result.
    let greeting: GreetResult =
        serde_json::from_value(ok_result(&outbox, 2).expect("greet response")).unwrap();
    assert_eq!(
        greeting,
        GreetResult {
            greeting: "Hello, Ada!".to_string()
        }
    );

    // shutdown returns a null result.
    assert_eq!(ok_result(&outbox, 3), Some(serde_json::Value::Null));

    assert_eq!(handled, 1, "the typed handler ran exactly once");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_params_return_invalid_params_and_a_later_request_succeeds() {
    let (outbox, handled) = drive(vec![
        initialize_request(1),
        // `name` must be a string; a number is malformed for GreetParams.
        request(2, "custom/greet", json!({ "name": 42 })),
        request(3, "custom/greet", json!({ "name": "Bob" })),
        notification("exit"),
    ])
    .await;

    assert_eq!(
        error_code(&outbox, 2),
        Some(-32602),
        "malformed params must return InvalidParams"
    );
    let greeting: GreetResult =
        serde_json::from_value(ok_result(&outbox, 3).expect("later greet response")).unwrap();
    assert_eq!(greeting.greeting, "Hello, Bob!");
    assert_eq!(
        handled, 1,
        "the handler ran only for the valid request, never for malformed params"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_before_initialize_returns_server_not_initialized() {
    let (outbox, handled) = drive(vec![
        request(1, "custom/greet", json!({ "name": "Ada" })),
        notification("exit"),
    ])
    .await;

    assert_eq!(
        error_code(&outbox, 1),
        Some(-32002),
        "a request before initialize must return ServerNotInitialized"
    );
    assert_eq!(handled, 0, "the handler must not run before initialize");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_initialize_is_rejected() {
    let (outbox, _handled) = drive(vec![
        initialize_request(1),
        initialize_request(2),
        notification("exit"),
    ])
    .await;

    assert!(
        ok_result(&outbox, 1).is_some(),
        "the first initialize succeeds"
    );
    assert_eq!(
        error_code(&outbox, 2),
        Some(-32600),
        "a second initialize must be refused"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_after_shutdown_returns_invalid_request() {
    let (outbox, handled) = drive(vec![
        initialize_request(1),
        request(2, "shutdown", json!(null)),
        request(3, "custom/greet", json!({ "name": "Ada" })),
        notification("exit"),
    ])
    .await;

    assert_eq!(
        error_code(&outbox, 3),
        Some(-32600),
        "a request after shutdown must return InvalidRequest"
    );
    assert_eq!(handled, 0, "the handler must not run after shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_request_returns_method_not_found() {
    let (outbox, _handled) = drive(vec![
        initialize_request(1),
        request(2, "custom/unregistered", json!({})),
        notification("exit"),
    ])
    .await;

    assert_eq!(
        error_code(&outbox, 2),
        Some(-32601),
        "an unregistered method must return MethodNotFound"
    );
}
