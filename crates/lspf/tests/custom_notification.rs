//! End-to-end coverage for the 0.2 typed custom notification slice (issue #40).
//!
//! A connection-owned [`Server`] registers one typed custom notification through
//! a marker implementing the re-exported `Notification` trait, and serves it over
//! an in-memory channel-backed [`Transport`]. The tests prove a valid
//! notification runs its handler and emits no response, and that a malformed
//! notification is dropped without invoking the handler or stopping later work.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

use lspf::types::notification::Notification;
use lspf::{
    Context, RawMessage, RequestId, Server, Transport, TransportError, TransportReader,
    TransportWriter,
};

// --- Custom notification marker ----------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct PingParams {
    note: String,
}

/// The marker fixes the wire method and parameter type dispatch uses. It
/// implements lspf's re-export of `lsp_types::notification::Notification`.
enum Ping {}

impl Notification for Ping {
    type Params = PingParams;
    const METHOD: &'static str = "custom/ping";
}

/// Application state shared as `Arc<S>` by every handler on the connection.
struct AppState {
    /// How many times the ping handler ran — proves malformed params never
    /// reach it and later valid notifications still do.
    handled: Arc<AtomicUsize>,
    /// Every note the handler observed, proving it decoded typed params.
    notes: Arc<Mutex<Vec<String>>>,
}

async fn ping(state: Arc<AppState>, _ctx: Context, params: PingParams) {
    state.handled.fetch_add(1, Ordering::SeqCst);
    state.notes.lock().await.push(params.note);
}

// --- In-memory transport -----------------------------------------------------

struct ChannelTransport {
    in_rx: mpsc::UnboundedReceiver<RawMessage>,
    outbox: Arc<Mutex<Vec<RawMessage>>>,
}

struct ChannelReader {
    in_rx: mpsc::UnboundedReceiver<RawMessage>,
}

struct ChannelWriter {
    outbox: Arc<Mutex<Vec<RawMessage>>>,
}

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            ChannelReader { in_rx: self.in_rx },
            ChannelWriter {
                outbox: self.outbox,
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
        self.outbox.lock().await.push(msg);
        Ok(())
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

/// Build the ping server, feed it `messages`, then close the transport so
/// `serve` returns once everything is processed. Returns the outbox and the
/// notes the handler observed, plus how many times it ran.
async fn drive(messages: Vec<RawMessage>) -> (Vec<RawMessage>, Vec<String>, usize) {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let transport = ChannelTransport {
        in_rx,
        outbox: outbox.clone(),
    };

    let handled = Arc::new(AtomicUsize::new(0));
    let notes = Arc::new(Mutex::new(Vec::new()));
    let server = Server::builder(AppState {
        handled: Arc::clone(&handled),
        notes: Arc::clone(&notes),
    })
    .notification::<Ping, _, _>(ping)
    .build()
    .expect("server builds");

    let handle = tokio::spawn(async move { server.serve(transport).await });

    for msg in messages {
        in_tx.send(msg).unwrap();
    }
    drop(in_tx); // peer disconnect → serve drains and returns

    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("serve returned within 2s")
        .expect("server task did not panic")
        .expect("serve ended cleanly");

    let outbox = Arc::try_unwrap(outbox).unwrap().into_inner();
    let notes = Arc::try_unwrap(notes).unwrap().into_inner();
    (outbox, notes, handled.load(Ordering::SeqCst))
}

fn responses(outbox: &[RawMessage]) -> Vec<&RawMessage> {
    outbox
        .iter()
        .filter(|m| matches!(m, RawMessage::Response { .. }))
        .collect()
}

// --- Tests -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_valid_notification_runs_its_handler_and_emits_no_response() {
    let (outbox, notes, handled) = drive(vec![
        initialize_request(1),
        notification("custom/ping", json!({ "note": "hello" })),
        exit(),
    ])
    .await;

    assert_eq!(handled, 1, "the typed handler ran once for the valid ping");
    assert_eq!(notes, vec!["hello".to_string()]);

    // Only `initialize` produced a response; the notification produced none.
    let responses = responses(&outbox);
    assert_eq!(
        responses.len(),
        1,
        "a notification emits no response, so only initialize replies"
    );
    assert_eq!(responses[0].id(), Some(&RequestId::Number(1)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_notification_is_dropped_and_later_messages_still_run() {
    let (outbox, notes, handled) = drive(vec![
        initialize_request(1),
        // `note` must be a string; a number is malformed for PingParams.
        notification("custom/ping", json!({ "note": 42 })),
        notification("custom/ping", json!({ "note": "after" })),
        exit(),
    ])
    .await;

    assert_eq!(
        handled, 1,
        "the handler ran only for the valid ping, never for malformed params"
    );
    assert_eq!(
        notes,
        vec!["after".to_string()],
        "the notification after a malformed one is still delivered"
    );
    // A malformed notification never becomes a wire response.
    assert_eq!(
        responses(&outbox).len(),
        1,
        "only initialize replies; neither ping produces a response"
    );
}

/// Initialize precedence (LSP §Initialize) on the notification side: there is
/// no Router before the initialize transaction commits, so a notification that
/// arrives first is dropped rather than queued for replay. The request side of
/// the same rule answers `ServerNotInitialized` instead (see
/// `custom_request::request_before_initialize_returns_server_not_initialized`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_notification_before_initialize_is_dropped_and_not_replayed() {
    let (outbox, notes, handled) = drive(vec![
        notification("custom/ping", json!({ "note": "too early" })),
        initialize_request(1),
        notification("custom/ping", json!({ "note": "after" })),
        exit(),
    ])
    .await;

    assert_eq!(
        handled, 1,
        "only the notification sent after initialize reaches the handler"
    );
    assert_eq!(
        notes,
        vec!["after".to_string()],
        "the dropped notification is never replayed once the Router exists"
    );
    assert_eq!(
        responses(&outbox).len(),
        1,
        "a dropped notification produces no response of its own"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unregistered_notification_is_ignored() {
    let (outbox, _notes, handled) = drive(vec![
        initialize_request(1),
        notification("custom/unregistered", json!({})),
        exit(),
    ])
    .await;

    assert_eq!(handled, 0, "no handler runs for an unregistered method");
    assert_eq!(
        responses(&outbox).len(),
        1,
        "an ignored notification produces no response"
    );
}
