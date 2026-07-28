//! End-to-end coverage for the 0.2 typed command slice (issue #40).
//!
//! Commands register beneath the built-in `workspace/executeCommand` entry and
//! dispatch by name with typed arguments, a typed result, a [`Context`], and a
//! cancellation token. Their names merge into one execute-command capability
//! advertised at initialization. These tests drive real envelopes through an
//! in-memory transport and inspect the outbox.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use tokio::sync::mpsc;

use lspf::{
    CancellationToken, Context, LspError, RawMessage, RequestId, Server, Transport, TransportError,
    TransportReader, TransportWriter,
};

/// Application state shared as `Arc<S>` by every handler on the connection.
struct AppState {
    /// Added to each sum, proving the command handler reads shared state.
    bias: i64,
}

/// A command taking two typed integer arguments and returning their sum plus a
/// state-derived bias. The cancellation token is exercised to prove it reaches
/// the handler.
async fn add(
    state: Arc<AppState>,
    _ctx: Context,
    args: (i64, i64),
    ct: CancellationToken,
) -> Result<i64, LspError> {
    assert!(!ct.is_cancelled(), "a fresh request token is not cancelled");
    Ok(args.0 + args.1 + state.bias)
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

fn execute_command(id: i32, command: &str, arguments: serde_json::Value) -> RawMessage {
    request(
        id,
        "workspace/executeCommand",
        json!({ "command": command, "arguments": arguments }),
    )
}

fn exit() -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed("exit"),
        params: Bytes::from_static(b"null"),
    }
}

/// Build the command server, feed it `messages`, then close the transport so
/// `serve` returns once everything is processed. Returns the outbox.
async fn drive(messages: Vec<RawMessage>) -> Vec<RawMessage> {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let transport = ChannelTransport { in_rx, out_tx };

    let server = Server::builder(AppState { bias: 100 })
        .command::<(i64, i64), i64, _, _>("math.add", add)
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
    drop(in_tx);

    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("serve returned within 2s")
        .expect("server task did not panic")
        .expect("serve ended cleanly");

    outbox.extend(std::iter::from_fn(|| out_rx.try_recv().ok()));
    outbox
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
async fn initialize_advertises_the_command_and_dispatches_it() {
    let outbox = drive(vec![
        initialize_request(1),
        execute_command(2, "math.add", json!([2, 3])),
        request(3, "shutdown", json!(null)),
        exit(),
    ])
    .await;

    // The registered command name contributes one execute-command capability.
    let init: lspf::types::InitializeResult =
        serde_json::from_value(ok_result(&outbox, 1).expect("initialize response")).unwrap();
    let provider = init
        .capabilities
        .execute_command_provider
        .expect("a registered command advertises execute-command support");
    assert_eq!(provider.commands, vec!["math.add".to_string()]);

    // The command dispatched by name with typed args, result, and state.
    assert_eq!(
        ok_result(&outbox, 2),
        Some(json!(105)),
        "2 + 3 + bias(100) routed through the typed command handler"
    );
    assert_eq!(ok_result(&outbox, 3), Some(serde_json::Value::Null));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_command_is_an_invalid_param() {
    let outbox = drive(vec![
        initialize_request(1),
        execute_command(2, "math.subtract", json!([2, 3])),
        exit(),
    ])
    .await;

    assert_eq!(
        error_code(&outbox, 2),
        Some(-32602),
        "an unregistered command name is an invalid parameter for executeCommand"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_command_arguments_return_invalid_params() {
    let outbox = drive(vec![
        initialize_request(1),
        // The typed args are (i64, i64); a string is malformed.
        execute_command(2, "math.add", json!(["two", 3])),
        // A well-formed call after a bad one still succeeds.
        execute_command(3, "math.add", json!([4, 5])),
        exit(),
    ])
    .await;

    assert_eq!(error_code(&outbox, 2), Some(-32602));
    assert_eq!(ok_result(&outbox, 3), Some(json!(109)));
}
