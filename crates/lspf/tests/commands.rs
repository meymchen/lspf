//! End-to-end coverage for the typed command wire contract (issues #40, #70).
//!
//! Commands register beneath the built-in `workspace/executeCommand` entry and
//! dispatch by name with typed arguments, a typed result, a [`ServerContext`], and a
//! request-scoped cancellation token. Tuple, struct, and `Vec` argument types
//! all decode from the complete LSP `arguments` array, an absent `arguments`
//! field is an empty array, decode failures and unknown names are
//! `InvalidParams` without invoking the handler, and the advertised command
//! list is de-duplicated and preserves registration order across static and
//! conditional registrations (ADR 0022) — verified byte-for-byte against
//! `fixtures/execute_command_provider_registration_order.json`. These tests
//! drive real envelopes through an in-memory transport and inspect the outbox.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{Mutex, mpsc, oneshot};

use lspf::{
    CancellationToken, LspError, RawMessage, RequestId, Server, ServerContext, Transport,
    TransportError, TransportReader, TransportWriter,
};

/// Application state shared as `Arc<S>` by every handler on the connection.
struct AppState {
    /// Added to each sum, proving the command handler reads shared state.
    bias: i64,
    /// Counts `math.add` invocations, proving a decode failure never calls it.
    add_calls: Arc<AtomicUsize>,
}

/// A command taking two typed integer arguments and returning their sum plus a
/// state-derived bias.
async fn add(
    state: Arc<AppState>,
    _ctx: ServerContext,
    args: (i64, i64),
    ct: CancellationToken,
) -> Result<i64, LspError> {
    state.add_calls.fetch_add(1, Ordering::SeqCst);
    assert!(!ct.is_cancelled(), "a fresh request token is not cancelled");
    Ok(args.0 + args.1 + state.bias)
}

/// A struct argument type: it must decode from the complete `arguments` array,
/// positionally, exactly like a tuple.
#[derive(Debug, Deserialize)]
struct RepeatArgs {
    text: String,
    times: usize,
}

/// A command taking a struct argument.
async fn repeat(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    args: RepeatArgs,
    _ct: CancellationToken,
) -> Result<String, LspError> {
    Ok(args.text.repeat(args.times))
}

/// A command taking a `Vec` argument.
async fn total(
    state: Arc<AppState>,
    _ctx: ServerContext,
    args: Vec<i64>,
    _ct: CancellationToken,
) -> Result<i64, LspError> {
    Ok(args.iter().sum::<i64>() + state.bias)
}

/// A no-op command used to fill the advertised command list.
async fn noop(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    _args: (),
    _ct: CancellationToken,
) -> Result<(), LspError> {
    Ok(())
}

/// State for the cancellation test: the command parks until released or
/// cancelled, reporting both milestones back to the test.
struct CancelState {
    started: Mutex<Option<oneshot::Sender<()>>>,
    cancellation_observed: Mutex<Option<oneshot::Sender<()>>>,
}

/// A command that parks until its request cancellation token fires.
async fn hang(
    state: Arc<CancelState>,
    _ctx: ServerContext,
    _args: Vec<serde_json::Value>,
    ct: CancellationToken,
) -> Result<(), LspError> {
    if let Some(started) = state.started.lock().await.take() {
        let _ = started.send(());
    }
    ct.cancelled().await;
    if let Some(observed) = state.cancellation_observed.lock().await.take() {
        let _ = observed.send(());
    }
    std::future::pending().await
}

/// The server every dispatch test shares: one tuple command, one struct
/// command, one `Vec` command, registered in a non-sorted order.
fn command_server(state: AppState) -> Server<AppState> {
    Server::builder(state)
        .command::<(i64, i64), i64, _, _>("math.add", add)
        .command::<RepeatArgs, String, _, _>("text.repeat", repeat)
        .command::<Vec<i64>, i64, _, _>("math.total", total)
        .build()
        .expect("server builds")
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

fn notification(method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Notification {
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

/// An execute-command request with the `arguments` field absent entirely.
fn execute_command_without_arguments(id: i32, command: &str) -> RawMessage {
    request(
        id,
        "workspace/executeCommand",
        json!({ "command": command }),
    )
}

fn exit() -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed("exit"),
        params: Bytes::from_static(b"null"),
    }
}

/// Drive `server` with `messages`, then close the transport so `serve` returns
/// once everything is processed. Returns the outbox.
async fn drive<S: Send + Sync + 'static>(
    server: Server<S>,
    messages: Vec<RawMessage>,
) -> Vec<RawMessage> {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let transport = ChannelTransport { in_rx, out_tx };

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

fn error_message(outbox: &[RawMessage], id: i32) -> Option<&str> {
    match response(outbox, id)? {
        RawMessage::Response { result: Err(e), .. } => Some(&e.message),
        _ => None,
    }
}

fn app_state() -> AppState {
    AppState {
        bias: 100,
        add_calls: Arc::new(AtomicUsize::new(0)),
    }
}

// --- Tests -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_advertises_the_command_and_dispatches_it() {
    let outbox = drive(
        command_server(app_state()),
        vec![
            initialize_request(1),
            execute_command(2, "math.add", json!([2, 3])),
            request(3, "shutdown", json!(null)),
            exit(),
        ],
    )
    .await;

    // The registered command names contribute one execute-command capability
    // in registration order, not sorted order.
    let init: lspf::types::InitializeResult =
        serde_json::from_value(ok_result(&outbox, 1).expect("initialize response")).unwrap();
    let provider = init
        .capabilities
        .execute_command_provider
        .expect("registered commands advertise execute-command support");
    assert_eq!(
        provider.commands,
        vec![
            "math.add".to_string(),
            "text.repeat".to_string(),
            "math.total".to_string()
        ]
    );

    // The command dispatched by name with typed args, result, and state.
    assert_eq!(
        ok_result(&outbox, 2),
        Some(json!(105)),
        "2 + 3 + bias(100) routed through the typed command handler"
    );
    assert_eq!(ok_result(&outbox, 3), Some(serde_json::Value::Null));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn struct_and_vec_arguments_decode_from_the_complete_arguments_array() {
    let outbox = drive(
        command_server(app_state()),
        vec![
            initialize_request(1),
            // A struct decodes from the complete array, fields in array order.
            execute_command(2, "text.repeat", json!(["ab", 3])),
            // A Vec decodes from the whole array of any length.
            execute_command(3, "math.total", json!([1, 2, 3, 4])),
            exit(),
        ],
    )
    .await;

    assert_eq!(ok_result(&outbox, 2), Some(json!("ababab")));
    assert_eq!(
        ok_result(&outbox, 3),
        Some(json!(110)),
        "1 + 2 + 3 + 4 + bias(100) decoded through a Vec argument"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_arguments_field_is_an_empty_array() {
    let outbox = drive(
        command_server(app_state()),
        vec![
            initialize_request(1),
            execute_command_without_arguments(2, "math.total"),
            exit(),
        ],
    )
    .await;

    assert_eq!(
        ok_result(&outbox, 2),
        Some(json!(100)),
        "no arguments field decodes as an empty Vec, leaving only the bias"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_command_is_an_invalid_param_naming_the_command() {
    let outbox = drive(
        command_server(app_state()),
        vec![
            initialize_request(1),
            execute_command(2, "math.subtract", json!([2, 3])),
            exit(),
        ],
    )
    .await;

    assert_eq!(
        error_code(&outbox, 2),
        Some(-32602),
        "an unregistered command name is an invalid parameter for executeCommand"
    );
    assert!(
        error_message(&outbox, 2)
            .expect("the error carries a message")
            .contains("math.subtract"),
        "the error message names the unknown command"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_command_arguments_return_invalid_params_without_invoking_the_handler() {
    let state = app_state();
    let add_calls = Arc::clone(&state.add_calls);
    let outbox = drive(
        command_server(state),
        vec![
            initialize_request(1),
            // The typed args are (i64, i64); a string is malformed.
            execute_command(2, "math.add", json!(["two", 3])),
            // A well-formed call after a bad one still succeeds.
            execute_command(3, "math.add", json!([4, 5])),
            exit(),
        ],
    )
    .await;

    assert_eq!(error_code(&outbox, 2), Some(-32602));
    assert_eq!(ok_result(&outbox, 3), Some(json!(109)));
    assert_eq!(
        add_calls.load(Ordering::SeqCst),
        1,
        "only the well-formed call invoked the typed handler"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_command_observes_cancellation_through_cancel_request() {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (started_tx, started_rx) = oneshot::channel();
    let (observed_tx, observed_rx) = oneshot::channel();
    let server = Server::builder(CancelState {
        started: Mutex::new(Some(started_tx)),
        cancellation_observed: Mutex::new(Some(observed_tx)),
    })
    .command::<Vec<serde_json::Value>, (), _, _>("test.hang", hang)
    .build()
    .expect("server builds");
    let serve = tokio::spawn(server.serve(ChannelTransport { in_rx, out_tx }));

    in_tx.send(initialize_request(1)).unwrap();
    let init = tokio::time::timeout(Duration::from_secs(2), out_rx.recv())
        .await
        .expect("initialize response within 2s")
        .expect("writer remained open");
    assert_eq!(init.id(), Some(&RequestId::Number(1)));

    in_tx
        .send(execute_command(2, "test.hang", json!([])))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), started_rx)
        .await
        .expect("command handler started before watchdog timeout")
        .expect("command handler started");
    in_tx
        .send(notification("$/cancelRequest", json!({ "id": 2 })))
        .unwrap();

    tokio::time::timeout(Duration::from_secs(2), observed_rx)
        .await
        .expect("command observed cancellation before watchdog timeout")
        .expect("command received its request cancellation token");
    let response = tokio::time::timeout(Duration::from_secs(2), out_rx.recv())
        .await
        .expect("cancelled response within 2s")
        .expect("writer remained open");
    assert!(
        matches!(
            response,
            RawMessage::Response {
                result: Err(ref error),
                ..
            } if error.code == -32800
        ),
        "the cancelled executeCommand request completes with RequestCancelled"
    );

    in_tx.send(exit()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), serve)
        .await
        .expect("serve returned within 2s")
        .expect("server task did not panic")
        .expect("serve ended cleanly");
    while let Ok(message) = out_rx.try_recv() {
        assert_ne!(
            message.id(),
            Some(&RequestId::Number(2)),
            "the cancelled request emitted more than one response"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advertised_commands_preserve_registration_order_across_static_and_conditional() {
    let server = Server::builder(app_state())
        .command::<(), (), _, _>("zeta.cmd", noop)
        .configure_initialize(|_params, registrar| {
            registrar.command::<(), (), _, _>("alpha.cmd", noop);
            registrar.command::<(), (), _, _>("mid.cmd", noop);
            Ok(())
        })
        .build()
        .expect("static and conditional commands build");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;

    let init: lspf::types::InitializeResult =
        serde_json::from_value(ok_result(&outbox, 1).expect("initialize response")).unwrap();
    let provider = init
        .capabilities
        .execute_command_provider
        .expect("registered commands advertise execute-command support");
    assert_eq!(
        provider.commands,
        vec![
            "zeta.cmd".to_string(),
            "alpha.cmd".to_string(),
            "mid.cmd".to_string()
        ],
        "the advertised list matches the frozen registry in registration order"
    );

    // Compare against the raw wire bytes, not a re-serialized typed value, so
    // a sorted, reordered, or renamed command field breaks here.
    let wire = match response(&outbox, 1).expect("initialize response") {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => String::from_utf8(bytes.to_vec()).unwrap(),
        other => panic!("expected a successful initialize response, got {other:?}"),
    };
    let fixture =
        include_str!("fixtures/execute_command_provider_registration_order.json").trim_end();
    assert!(
        wire.contains(&format!("\"executeCommandProvider\":{fixture}")),
        "the advertised executeCommandProvider on the wire must stay byte-stable \
         in registration order; update the fixture only with a deliberate \
         capability change.\nwire: {wire}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_command_registration_errors_fail_the_initialize_transaction() {
    // A conditional duplicate of a static command name.
    let duplicate = Server::builder(app_state())
        .command::<(), (), _, _>("my.cmd", noop)
        .configure_initialize(|_params, registrar| {
            registrar.command::<(), (), _, _>("my.cmd", noop);
            Ok(())
        })
        .build()
        .expect("build does not run the conditional transaction");
    let outbox = drive(duplicate, vec![initialize_request(1)]).await;
    assert_eq!(
        error_code(&outbox, 1),
        Some(-32603),
        "a duplicate conditional command name fails the initialize transaction"
    );

    // A conditional empty command name.
    let empty = Server::builder(app_state())
        .configure_initialize(|_params, registrar| {
            registrar.command::<(), (), _, _>("", noop);
            Ok(())
        })
        .build()
        .expect("build does not run the conditional transaction");
    let outbox = drive(empty, vec![initialize_request(1)]).await;
    assert_eq!(
        error_code(&outbox, 1),
        Some(-32603),
        "an empty conditional command name fails the initialize transaction"
    );
}
