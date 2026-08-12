//! End-to-end coverage for post-mutation document hooks (issue #49).
//!
//! `textDocument/didOpen`, `didChange`, and `didClose` are protocol built-ins:
//! the engine decodes and mutates the connection-owned [`Documents`]
//! serially, and only then does the user's registered hook enter the Service
//! stack. These tests drive a connection-owned [`Server`] over an in-memory
//! transport and prove the hook observes post-mutation state through the
//! read-only `DocumentsView`, cannot suppress or roll back the built-in, and is
//! skipped — without ending the connection — when decode or built-in validation
//! fails. `willSave` and `didSave` have no document mutation, but still pass
//! through protocol-owned configuration and validation before their hooks.

use std::borrow::Cow;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

use lspf::types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    WillSaveTextDocument,
};
use lspf::types::request::Request;
use lspf::types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, Position, SaveOptions, TextDocumentSyncKind,
    TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TextEdit, Uri,
    WillSaveTextDocumentParams,
};
use lspf::{
    BuildError, CancellationToken, Context, LspError, RawMessage, RequestId, Server, Transport,
    TransportError, TransportReader, TransportWriter,
};

// --- What each hook observed -------------------------------------------------

/// One hook invocation, recorded as the state the hook saw *after* the built-in
/// mutation ran. Every field is read through `ctx.documents()`, so the record is
/// exactly what a user handler can observe.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Open {
        text: Option<String>,
        version: Option<i32>,
    },
    Change {
        text: Option<String>,
        version: Option<i32>,
    },
    Close {
        still_present: bool,
    },
    Save {
        still_present: bool,
        text: Option<String>,
    },
    WillSave {
        still_present: bool,
    },
}

type Log = Arc<Mutex<Vec<Seen>>>;

struct AppState {
    seen: Log,
}

fn uri(s: &str) -> Uri {
    Uri::from_str(s).expect("test URIs are valid")
}

async fn on_did_open(state: Arc<AppState>, ctx: Context, params: DidOpenTextDocumentParams) {
    let doc = ctx.documents().get(&params.text_document.uri);
    state.seen.lock().unwrap().push(Seen::Open {
        text: doc.as_ref().map(|d| d.text()),
        version: doc.as_ref().map(|d| d.version()),
    });
}

async fn on_did_change(state: Arc<AppState>, ctx: Context, params: DidChangeTextDocumentParams) {
    let doc = ctx.documents().get(&params.text_document.uri);
    state.seen.lock().unwrap().push(Seen::Change {
        text: doc.as_ref().map(|d| d.text()),
        version: doc.as_ref().map(|d| d.version()),
    });
}

async fn on_did_close(state: Arc<AppState>, ctx: Context, params: DidCloseTextDocumentParams) {
    let still_present = ctx.documents().get(&params.text_document.uri).is_some();
    state
        .seen
        .lock()
        .unwrap()
        .push(Seen::Close { still_present });
}

async fn on_did_save(state: Arc<AppState>, ctx: Context, params: DidSaveTextDocumentParams) {
    let still_present = ctx.documents().get(&params.text_document.uri).is_some();
    state.seen.lock().unwrap().push(Seen::Save {
        still_present,
        text: params.text,
    });
}

async fn on_will_save(state: Arc<AppState>, ctx: Context, params: WillSaveTextDocumentParams) {
    let still_present = ctx.documents().get(&params.text_document.uri).is_some();
    state
        .seen
        .lock()
        .unwrap()
        .push(Seen::WillSave { still_present });
}

async fn on_will_save_wait_until(
    _state: Arc<AppState>,
    _ctx: Context,
    _params: WillSaveTextDocumentParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<TextEdit>>, LspError> {
    Ok(Some(Vec::new()))
}

// --- A custom request that reads the documents through the view --------------

#[derive(Debug, Serialize, Deserialize)]
struct ProbeParams {
    uri: String,
    /// Optional position to convert with the connection's negotiated encoding.
    #[serde(default)]
    position: Option<Position>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProbeResult {
    text: Option<String>,
    version: Option<i32>,
    offset: Option<usize>,
    utf8_encoding: bool,
}

/// A custom request whose only job is to report what `ctx.documents()` sees, so
/// a test can observe the documents from a message that arrives *after* the
/// document notifications.
enum Probe {}

impl Request for Probe {
    type Params = ProbeParams;
    type Result = ProbeResult;
    const METHOD: &'static str = "custom/probe";
}

async fn probe(
    _state: Arc<AppState>,
    ctx: Context,
    params: ProbeParams,
    _ct: CancellationToken,
) -> Result<ProbeResult, LspError> {
    let documents = ctx.documents();
    let uri = uri(&params.uri);
    let doc = documents.get(&uri);
    Ok(ProbeResult {
        text: doc.as_ref().map(|d| d.text()),
        version: doc.as_ref().map(|d| d.version()),
        offset: params
            .position
            .and_then(|position| documents.position_to_offset(&uri, position)),
        utf8_encoding: documents.position_encoding() == lspf::PositionEncoding::Utf8,
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

/// An `initialize` whose client offers UTF-8, so the connection negotiates it
/// (ADR 0016) and the view reports byte offsets.
fn initialize_utf8_request(id: i32) -> RawMessage {
    request(
        id,
        "initialize",
        json!({
            "processId": null,
            "rootUri": null,
            "capabilities": { "general": { "positionEncodings": ["utf-8"] } }
        }),
    )
}

fn notification(method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn did_open(uri: &str, text: &str) -> RawMessage {
    notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "plaintext",
                "version": 1,
                "text": text
            }
        }),
    )
}

fn did_change(uri: &str, version: i32, start: u32, end: u32, text: &str) -> RawMessage {
    notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{
                "range": {
                    "start": { "line": 0, "character": start },
                    "end": { "line": 0, "character": end }
                },
                "text": text
            }]
        }),
    )
}

fn did_change_full(uri: &str, version: i32, text: &str) -> RawMessage {
    notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": text }]
        }),
    )
}

fn did_close(uri: &str) -> RawMessage {
    notification(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": uri } }),
    )
}

fn did_save(uri: &str) -> RawMessage {
    notification(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": uri } }),
    )
}

fn did_save_with_text(uri: &str, text: &str) -> RawMessage {
    notification(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": uri }, "text": text }),
    )
}

fn will_save(uri: &str) -> RawMessage {
    notification(
        "textDocument/willSave",
        json!({ "textDocument": { "uri": uri }, "reason": 1 }),
    )
}

fn probe_request(id: i32, uri: &str) -> RawMessage {
    request(id, "custom/probe", json!({ "uri": uri }))
}

// --- Harness -----------------------------------------------------------------

/// Every document hook plus the probe request, so one server can observe all
/// four notifications.
fn observing_server(seen: &Log) -> Server<AppState> {
    Server::builder(AppState {
        seen: Arc::clone(seen),
    })
    .notification::<DidOpenTextDocument, _, _>(on_did_open)
    .notification::<DidChangeTextDocument, _, _>(on_did_change)
    .notification::<DidCloseTextDocument, _, _>(on_did_close)
    .notification::<DidSaveTextDocument, _, _>(on_did_save)
    .request::<Probe, _, _>(probe)
    .build()
    .expect("one hook per built-in notification is a valid registration set")
}

/// Serve `server` over an in-memory transport, feeding `messages` one at a time
/// and awaiting each request's response before sending the next, so ordering is
/// observable. Closing the peer end then drains the connection.
async fn drive(server: Server<AppState>, messages: Vec<RawMessage>) -> Vec<RawMessage> {
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
    drop(in_tx); // peer disconnect → serve drains and returns

    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("serve returned within 2s")
        .expect("server task did not panic")
        .expect("serve ended cleanly");

    outbox.extend(std::iter::from_fn(|| out_rx.try_recv().ok()));
    outbox
}

fn ok_result<T: serde::de::DeserializeOwned>(outbox: &[RawMessage], id: i32) -> T {
    let response = outbox
        .iter()
        .find(
            |m| matches!(m, RawMessage::Response { id: rid, .. } if *rid == RequestId::Number(id)),
        )
        .expect("the request was answered");
    match response {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => serde_json::from_slice(bytes).expect("the result decodes"),
        other => panic!("expected a success response, got {other:?}"),
    }
}

fn probed(outbox: &[RawMessage], id: i32) -> ProbeResult {
    ok_result(outbox, id)
}

// --- Tests -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_document_hook_observes_the_post_mutation_documents() {
    let seen: Log = Arc::default();
    drive(
        observing_server(&seen),
        vec![
            initialize_request(1),
            did_open("file:///hooks.txt", "hello world"),
            did_change("file:///hooks.txt", 2, 6, 11, "lspf"),
            did_save("file:///hooks.txt"),
            did_close("file:///hooks.txt"),
        ],
    )
    .await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            Seen::Open {
                text: Some("hello world".to_string()),
                version: Some(1),
            },
            Seen::Change {
                text: Some("hello lspf".to_string()),
                version: Some(2),
            },
            // didSave has no document mutation, so the document is untouched
            // and still open when the post-validation hook runs.
            Seen::Save {
                still_present: true,
                text: None,
            },
            Seen::Close {
                still_present: false
            },
        ],
        "every hook runs once, in receipt order, observing post-mutation state"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_built_in_mutation_runs_without_any_registered_hook() {
    let seen: Log = Arc::default();
    // Only the probe request is registered — no document hook at all.
    let server = Server::builder(AppState {
        seen: Arc::clone(&seen),
    })
    .request::<Probe, _, _>(probe)
    .build()
    .expect("a lone custom request builds");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            did_open("file:///no-hook.txt", "hello world"),
            did_change("file:///no-hook.txt", 2, 6, 11, "lspf"),
            probe_request(2, "file:///no-hook.txt"),
        ],
    )
    .await;

    let probed = probed(&outbox, 2);
    assert_eq!(
        probed.text.as_deref(),
        Some("hello lspf"),
        "document mutation is a built-in, not something a hook opts into"
    );
    assert_eq!(probed.version, Some(2));
    assert!(
        seen.lock().unwrap().is_empty(),
        "no hook was registered, so nothing was observed"
    );
}

/// A hook cannot suppress the built-in: it runs strictly after the mutation has
/// landed, and even a panicking hook — isolated by the framework's outermost
/// Layer — leaves the documents mutated and the connection able to serve later
/// messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_hook_cannot_suppress_or_roll_back_the_mutation() {
    async fn panicking_hook(
        _state: Arc<AppState>,
        _ctx: Context,
        _params: DidOpenTextDocumentParams,
    ) {
        panic!("a hook must not be able to undo the built-in mutation");
    }

    let server = Server::builder(AppState {
        seen: Arc::default(),
    })
    .notification::<DidOpenTextDocument, _, _>(panicking_hook)
    .request::<Probe, _, _>(probe)
    .build()
    .expect("server builds");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            did_open("file:///panics.txt", "still here"),
            probe_request(2, "file:///panics.txt"),
        ],
    )
    .await;

    assert_eq!(
        probed(&outbox, 2).text.as_deref(),
        Some("still here"),
        "the mutation survives a hook that panics after it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_document_params_skip_the_hook_and_later_messages_still_run() {
    let seen: Log = Arc::default();
    let outbox = drive(
        observing_server(&seen),
        vec![
            initialize_request(1),
            // `version` must be an integer, so these params never decode.
            notification(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": "file:///malformed.txt",
                        "languageId": "plaintext",
                        "version": "not-a-number",
                        "text": "ignored"
                    }
                }),
            ),
            did_open("file:///after.txt", "later work still happens"),
            probe_request(2, "file:///malformed.txt"),
        ],
    )
    .await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![Seen::Open {
            text: Some("later work still happens".to_string()),
            version: Some(1),
        }],
        "a decode failure skips the hook; the next notification still runs it"
    );
    assert_eq!(
        probed(&outbox, 2).text,
        None,
        "nothing was opened for the malformed notification"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_change_skips_the_hook_and_leaves_the_document_intact() {
    let seen: Log = Arc::default();
    let outbox = drive(
        observing_server(&seen),
        vec![
            initialize_request(1),
            did_open("file:///invalid-change.txt", "hello world"),
            // A range whose end precedes its start fails built-in validation.
            did_change("file:///invalid-change.txt", 2, 11, 6, "lspf"),
            probe_request(2, "file:///invalid-change.txt"),
        ],
    )
    .await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![Seen::Open {
            text: Some("hello world".to_string()),
            version: Some(1),
        }],
        "built-in validation failure skips the change hook"
    );
    let probed = probed(&outbox, 2);
    assert_eq!(
        probed.text.as_deref(),
        Some("hello world"),
        "a rejected change leaves the document as it was"
    );
    assert_eq!(
        probed.version,
        Some(1),
        "a rejected change does not advance the version"
    );
}

/// A rejected change in the middle of a batch must not leave a half-applied
/// revision behind: the whole notification is refused, so the document stays at
/// the revision the last accepted notification produced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_batch_applies_all_or_nothing() {
    let seen: Log = Arc::default();
    let outbox = drive(
        observing_server(&seen),
        vec![
            initialize_request(1),
            did_open("file:///batch.txt", "hello world"),
            // The first edit is applicable on its own; the second is reversed.
            notification(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": "file:///batch.txt", "version": 2 },
                    "contentChanges": [
                        {
                            "range": {
                                "start": { "line": 0, "character": 6 },
                                "end": { "line": 0, "character": 11 }
                            },
                            "text": "lspf"
                        },
                        {
                            "range": {
                                "start": { "line": 0, "character": 5 },
                                "end": { "line": 0, "character": 0 }
                            },
                            "text": "!"
                        }
                    ]
                }),
            ),
            probe_request(2, "file:///batch.txt"),
        ],
    )
    .await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![Seen::Open {
            text: Some("hello world".to_string()),
            version: Some(1),
        }],
        "a rejected batch skips the change hook"
    );
    let probed = probed(&outbox, 2);
    assert_eq!(
        probed.text.as_deref(),
        Some("hello world"),
        "the batch's first edit is rolled back with the rest of it"
    );
    assert_eq!(
        probed.version,
        Some(1),
        "a rejected batch does not advance the version"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_batch_composes_its_edits_in_order() {
    let seen: Log = Arc::default();
    drive(
        observing_server(&seen),
        vec![
            initialize_request(1),
            did_open("file:///compose.txt", "hello world"),
            notification(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": "file:///compose.txt", "version": 2 },
                    "contentChanges": [
                        // "hello world" -> "hello lspf"
                        {
                            "range": {
                                "start": { "line": 0, "character": 6 },
                                "end": { "line": 0, "character": 11 }
                            },
                            "text": "lspf"
                        },
                        // "hello lspf" -> "hi lspf", ranged against the result
                        // of the edit before it.
                        {
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 5 }
                            },
                            "text": "hi"
                        }
                    ]
                }),
            ),
        ],
    )
    .await;

    assert_eq!(
        seen.lock().unwrap()[1],
        Seen::Change {
            text: Some("hi lspf".to_string()),
            version: Some(2),
        },
        "the hook observes the whole batch applied in receipt order, once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_a_document_that_was_never_opened_still_runs_the_hook() {
    let seen: Log = Arc::default();
    drive(
        observing_server(&seen),
        vec![
            initialize_request(1),
            did_close("file:///never-opened.txt"),
            did_open("file:///opened.txt", "x"),
            did_close("file:///opened.txt"),
        ],
    )
    .await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            // Nothing was there to remove, so the hook observes the same
            // absence a real close would have left behind.
            Seen::Close {
                still_present: false
            },
            Seen::Open {
                text: Some("x".to_string()),
                version: Some(1),
            },
            Seen::Close {
                still_present: false
            },
        ],
        "a close with nothing to remove is not a validation failure"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_view_converts_positions_with_the_negotiated_encoding() {
    let seen: Log = Arc::default();
    let outbox = drive(
        observing_server(&seen),
        vec![
            initialize_utf8_request(1),
            // "héllo" — 'é' is two UTF-8 bytes, so byte 3 is the second 'l'
            // under UTF-8 but would be character 4 under UTF-16.
            did_open("file:///encoded.txt", "héllo"),
            request(
                2,
                "custom/probe",
                json!({
                    "uri": "file:///encoded.txt",
                    "position": { "line": 0, "character": 3 }
                }),
            ),
        ],
    )
    .await;

    let probed = probed(&outbox, 2);
    assert!(
        probed.utf8_encoding,
        "the view reports the encoding the connection negotiated"
    );
    assert_eq!(
        probed.offset,
        Some(3),
        "under UTF-8 `character` is a byte offset within the line"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_advertises_the_built_in_incremental_document_sync() {
    let seen: Log = Arc::default();
    // No document hook is registered: the sync capability describes the
    // protocol built-in the engine always performs, not a user registration.
    let server = Server::builder(AppState {
        seen: Arc::clone(&seen),
    })
    .build()
    .expect("an empty server builds");
    let outbox = drive(server, vec![initialize_request(1)]).await;

    let init: lspf::types::InitializeResult = ok_result(&outbox, 1);
    assert_eq!(
        init.capabilities.text_document_sync,
        Some(lspf::types::TextDocumentSyncCapability::Kind(
            lspf::types::TextDocumentSyncKind::INCREMENTAL
        )),
        "a client only sends didOpen/didChange/didClose to a server that \
         advertises the sync kind the engine's built-ins implement"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_advertises_configured_full_document_sync() {
    let server = Server::builder(AppState {
        seen: Arc::default(),
    })
    .text_document_sync(TextDocumentSyncOptions {
        open_close: Some(true),
        change: Some(TextDocumentSyncKind::FULL),
        will_save: Some(false),
        will_save_wait_until: Some(false),
        save: Some(false.into()),
    })
    .build()
    .expect("full document synchronization is a valid configuration");

    let outbox = drive(server, vec![initialize_request(1)]).await;
    let wire = serde_json::to_string_pretty(
        &ok_result::<serde_json::Value>(&outbox, 1)["capabilities"]["textDocumentSync"],
    )
    .expect("the capability serializes");
    assert_eq!(
        wire,
        include_str!("fixtures/full_document_sync_capability.json").trim_end(),
        "configured full synchronization stays byte-stable"
    );
}

#[test]
fn duplicate_document_hook_registration_fails_during_build() {
    let err = Server::builder(AppState {
        seen: Arc::default(),
    })
    .notification::<DidOpenTextDocument, _, _>(on_did_open)
    .notification::<DidOpenTextDocument, _, _>(on_did_open)
    .build()
    .err()
    .expect("a built-in notification takes at most one hook");
    assert_eq!(
        err,
        BuildError::DuplicateMethod("textDocument/didOpen".to_string())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_none_ignores_document_notifications_and_keeps_the_session_running() {
    let seen: Log = Arc::default();
    let server = Server::builder(AppState {
        seen: Arc::clone(&seen),
    })
    .text_document_sync(TextDocumentSyncOptions {
        change: Some(TextDocumentSyncKind::NONE),
        ..TextDocumentSyncOptions::default()
    })
    .notification::<DidOpenTextDocument, _, _>(on_did_open)
    .notification::<DidChangeTextDocument, _, _>(on_did_change)
    .notification::<DidCloseTextDocument, _, _>(on_did_close)
    .request::<Probe, _, _>(probe)
    .build()
    .expect("hooks may be registered even when synchronization is disabled");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            did_open("file:///none.txt", "before"),
            did_change("file:///none.txt", 2, 0, 1, "X"),
            did_close("file:///none.txt"),
            probe_request(2, "file:///none.txt"),
        ],
    )
    .await;

    assert!(seen.lock().unwrap().is_empty(), "disabled hooks never run");
    assert_eq!(probed(&outbox, 2).text, None, "no document was mutated");
    let init: lspf::types::InitializeResult = ok_result(&outbox, 1);
    assert_eq!(
        init.capabilities.text_document_sync,
        Some(lspf::types::TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::NONE
        ))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_sync_rejects_a_range_batch_atomically_and_skips_the_hook() {
    let seen: Log = Arc::default();
    let server = Server::builder(AppState {
        seen: Arc::clone(&seen),
    })
    .text_document_sync(TextDocumentSyncOptions {
        change: Some(TextDocumentSyncKind::FULL),
        ..TextDocumentSyncOptions::default()
    })
    .notification::<DidOpenTextDocument, _, _>(on_did_open)
    .notification::<DidChangeTextDocument, _, _>(on_did_change)
    .request::<Probe, _, _>(probe)
    .build()
    .expect("full synchronization builds");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            did_open("file:///full.txt", "before"),
            did_change("file:///full.txt", 2, 0, 1, "X"),
            probe_request(2, "file:///full.txt"),
            did_change_full("file:///full.txt", 3, "after"),
            probe_request(3, "file:///full.txt"),
        ],
    )
    .await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            Seen::Open {
                text: Some("before".to_string()),
                version: Some(1),
            },
            Seen::Change {
                text: Some("after".to_string()),
                version: Some(3),
            },
        ],
        "the rejected change hook is skipped"
    );
    let document = probed(&outbox, 2);
    assert_eq!(document.text.as_deref(), Some("before"));
    assert_eq!(document.version, Some(1));
    let replaced = probed(&outbox, 3);
    assert_eq!(replaced.text.as_deref(), Some("after"));
    assert_eq!(replaced.version, Some(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_sync_accepts_full_replacements() {
    let seen: Log = Arc::default();
    let outbox = drive(
        observing_server(&seen),
        vec![
            initialize_request(1),
            did_open("file:///replace.txt", "before"),
            did_change_full("file:///replace.txt", 2, "after"),
            probe_request(2, "file:///replace.txt"),
        ],
    )
    .await;
    let document = probed(&outbox, 2);
    assert_eq!(document.text.as_deref(), Some("after"));
    assert_eq!(document.version, Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_close_false_skips_built_ins_even_when_a_hook_is_registered() {
    let seen: Log = Arc::default();
    let server = Server::builder(AppState {
        seen: Arc::clone(&seen),
    })
    .text_document_sync(TextDocumentSyncOptions {
        open_close: Some(false),
        ..TextDocumentSyncOptions::default()
    })
    .notification::<DidOpenTextDocument, _, _>(on_did_open)
    .notification::<DidCloseTextDocument, _, _>(on_did_close)
    .request::<Probe, _, _>(probe)
    .build()
    .expect("open/close may be disabled with a dormant hook");
    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            did_open("file:///closed.txt", "ignored"),
            did_close("file:///closed.txt"),
            probe_request(2, "file:///closed.txt"),
        ],
    )
    .await;
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(probed(&outbox, 2).text, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn save_hooks_are_validated_then_run_and_are_inferred_into_capabilities() {
    let seen: Log = Arc::default();
    let server = Server::builder(AppState {
        seen: Arc::clone(&seen),
    })
    .text_document_sync(TextDocumentSyncOptions {
        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
            include_text: Some(true),
        })),
        ..TextDocumentSyncOptions::default()
    })
    .notification::<WillSaveTextDocument, _, _>(on_will_save)
    .notification::<DidSaveTextDocument, _, _>(on_did_save)
    .build()
    .expect("typed save hooks agree with the configured options");
    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            did_open("file:///save.txt", "saved"),
            will_save("file:///save.txt"),
            did_save("file:///save.txt"),
            did_save_with_text("file:///save.txt", "saved"),
        ],
    )
    .await;
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            Seen::WillSave {
                still_present: true
            },
            Seen::Save {
                still_present: true,
                text: Some("saved".to_string()),
            },
        ]
    );
    let init: lspf::types::InitializeResult = ok_result(&outbox, 1);
    let lspf::types::TextDocumentSyncCapability::Options(options) = init
        .capabilities
        .text_document_sync
        .expect("sync capability")
    else {
        panic!("save hooks require the options capability form");
    };
    assert_eq!(options.will_save, Some(true));
    assert_eq!(
        options.save,
        Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
            include_text: Some(true),
        }))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn will_save_wait_until_descriptor_contributes_the_sync_capability() {
    let server = Server::builder(AppState {
        seen: Arc::default(),
    })
    .feature(
        lspf::features::will_save_wait_until(),
        on_will_save_wait_until,
    )
    .build()
    .expect("the typed willSaveWaitUntil feature builds");
    let outbox = drive(server, vec![initialize_request(1)]).await;
    let init: lspf::types::InitializeResult = ok_result(&outbox, 1);
    let lspf::types::TextDocumentSyncCapability::Options(options) = init
        .capabilities
        .text_document_sync
        .expect("sync capability")
    else {
        panic!("willSaveWaitUntil requires the options capability form");
    };
    assert_eq!(options.will_save_wait_until, Some(true));
}

#[test]
fn explicit_false_save_fields_conflict_with_typed_registrations() {
    let save = Server::builder(AppState {
        seen: Arc::default(),
    })
    .text_document_sync(TextDocumentSyncOptions {
        save: Some(false.into()),
        ..TextDocumentSyncOptions::default()
    })
    .notification::<DidSaveTextDocument, _, _>(on_did_save)
    .build()
    .err()
    .expect("didSave conflicts with save: false");
    assert_eq!(
        save,
        BuildError::ConflictingCapability {
            field: "textDocumentSync.save"
        }
    );

    let will_save = Server::builder(AppState {
        seen: Arc::default(),
    })
    .text_document_sync(TextDocumentSyncOptions {
        will_save: Some(false),
        ..TextDocumentSyncOptions::default()
    })
    .notification::<WillSaveTextDocument, _, _>(on_will_save)
    .build()
    .err()
    .expect("willSave conflicts with willSave: false");
    assert_eq!(
        will_save,
        BuildError::ConflictingCapability {
            field: "textDocumentSync.willSave"
        }
    );

    let wait_until = Server::builder(AppState {
        seen: Arc::default(),
    })
    .text_document_sync(TextDocumentSyncOptions {
        will_save_wait_until: Some(false),
        ..TextDocumentSyncOptions::default()
    })
    .feature(
        lspf::features::will_save_wait_until(),
        on_will_save_wait_until,
    )
    .build()
    .err()
    .expect("willSaveWaitUntil conflicts with its explicit false field");
    assert_eq!(
        wait_until,
        BuildError::ConflictingCapability {
            field: "textDocumentSync.willSaveWaitUntil"
        }
    );
}

#[test]
fn sync_none_conflicts_with_save_registrations() {
    let err = Server::builder(AppState {
        seen: Arc::default(),
    })
    .text_document_sync(TextDocumentSyncOptions {
        change: Some(TextDocumentSyncKind::NONE),
        ..TextDocumentSyncOptions::default()
    })
    .feature(
        lspf::features::will_save_wait_until(),
        on_will_save_wait_until,
    )
    .build()
    .err()
    .expect("no synchronization cannot expose a save request route");

    assert_eq!(
        err,
        BuildError::ConflictingCapability {
            field: "textDocumentSync.willSaveWaitUntil"
        }
    );
}
