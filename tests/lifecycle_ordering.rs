//! Lifecycle-ordering guarantees under concurrent dispatch (issue #4).
//!
//! Two LSP-spec invariants must survive the concurrent dispatcher:
//!
//! 1. **Initialize precedence** — before `initialize` completes, any
//!    other inbound request is answered `ServerNotInitialized` without
//!    spawning a handler, and any notification other than `initialized`
//!    / `exit` is dropped.
//! 2. **Exit aborts in-flight work** — an `exit` notification aborts
//!    every in-flight handler rather than awaiting it.
//!
//! Like `cancellation.rs`, these drive the dispatcher through an
//! in-process channel-backed [`Transport`] so messages can be injected
//! out of band and the outbox inspected directly.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use tokio::sync::mpsc;

use lspf::types::{
    Diagnostic, DiagnosticSeverity, DidOpenTextDocumentParams, Position, PublishDiagnosticsParams,
    Range,
};
use lspf::{
    Context, LanguageServer, RawMessage, RequestId, Transport, TransportError, TransportReader,
    TransportWriter,
};

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
        self.outbox.lock().unwrap().push(msg);
        Ok(())
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

/// A server whose every built-in override has an observable effect, so a
/// test can tell whether a handler actually ran. `didOpen` publishes a
/// diagnostic; `initialize`/`shutdown` use the default success replies.
struct Probe;

impl LanguageServer for Probe {
    async fn text_document_did_open(&self, ctx: &Context, params: DidOpenTextDocumentParams) {
        ctx.publish_diagnostics(PublishDiagnosticsParams {
            uri: params.text_document.uri,
            version: Some(params.text_document.version),
            diagnostics: vec![Diagnostic {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 0,
                        character: 0,
                    },
                },
                severity: Some(DiagnosticSeverity::INFORMATION),
                source: Some("lifecycle-probe".into()),
                message: "didOpen ran".into(),
                ..Diagnostic::default()
            }],
        });
    }
}

fn initialize_request(id: i32) -> RawMessage {
    let params = json!({ "processId": null, "rootUri": null, "capabilities": {} });
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed("initialize"),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn request(id: i32, method: &'static str) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from_static(b"{}"),
    }
}

fn notification(method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn did_open_notification(uri: &str) -> RawMessage {
    notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "plaintext",
                "version": 1,
                "text": "hello"
            }
        }),
    )
}

fn has_publish_diagnostics(outbox: &[RawMessage]) -> bool {
    outbox.iter().any(|m| {
        matches!(
            m,
            RawMessage::Notification { method, .. }
                if method == "textDocument/publishDiagnostics"
        )
    })
}

async fn wait_for_response(
    outbox: &Arc<Mutex<Vec<RawMessage>>>,
    id: &RequestId,
    deadline: Duration,
) {
    let start = std::time::Instant::now();
    loop {
        let found = outbox
            .lock()
            .unwrap()
            .iter()
            .any(|m| matches!(m, RawMessage::Response { id: rid, .. } if rid == id));
        if found {
            return;
        }
        assert!(
            start.elapsed() < deadline,
            "no response for {id:?} within {deadline:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn error_code(outbox: &[RawMessage], id: &RequestId) -> Option<i32> {
    outbox.iter().find_map(|m| match m {
        RawMessage::Response { id: rid, result } if rid == id => match result {
            Err(e) => Some(e.code),
            Ok(_) => None,
        },
        _ => None,
    })
}

/// Feed a single message into a freshly-started (uninitialized) server,
/// then close the transport so `serve` returns once the message — and
/// any handler it may have spawned — is fully processed. Returns the
/// outbox.
async fn drive_uninitialized(msg: RawMessage) -> Vec<RawMessage> {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let transport = ChannelTransport {
        in_rx,
        outbox: outbox.clone(),
    };

    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve(Probe, transport).await;
    });

    in_tx.send(msg).unwrap();
    drop(in_tx); // peer disconnect → serve drains and returns

    tokio::time::timeout(Duration::from_secs(2), server_handle)
        .await
        .expect("serve returned within 2s")
        .expect("server task did not panic");

    let v = outbox.lock().unwrap().clone();
    v
}

/// A server whose `didOpen` sleeps a long time before publishing, so a
/// test can tell whether an in-flight handler was aborted (no publish,
/// prompt return) or awaited to completion (publish after the sleep).
struct SlowOpen;

const SLOW: Duration = Duration::from_secs(2);

impl LanguageServer for SlowOpen {
    async fn text_document_did_open(&self, ctx: &Context, params: DidOpenTextDocumentParams) {
        tokio::time::sleep(SLOW).await;
        ctx.publish_diagnostics(PublishDiagnosticsParams {
            uri: params.text_document.uri,
            version: Some(params.text_document.version),
            diagnostics: vec![],
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_aborts_in_flight_handler() {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let transport = ChannelTransport {
        in_rx,
        outbox: outbox.clone(),
    };

    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve(SlowOpen, transport).await;
    });

    // Reach Running so the didOpen isn't gated, then put the slow handler
    // in flight.
    in_tx.send(initialize_request(1)).unwrap();
    wait_for_response(&outbox, &RequestId::Number(1), Duration::from_millis(500)).await;
    in_tx.send(did_open_notification("file:///slow")).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await; // let it reach its sleep

    // `exit` must abort the in-flight handler, not await its 2s sleep.
    let exit_sent = std::time::Instant::now();
    in_tx.send(notification("exit", json!(null))).unwrap();

    tokio::time::timeout(Duration::from_millis(500), server_handle)
        .await
        .expect("serve returned within 500ms — exit aborted the in-flight handler")
        .expect("server task did not panic");

    assert!(
        exit_sent.elapsed() < SLOW,
        "exit took {:?}, which means it awaited the slow handler instead of aborting it",
        exit_sent.elapsed()
    );
    assert!(
        !has_publish_diagnostics(&outbox.lock().unwrap()),
        "aborted handler must not have published its diagnostic"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notification_before_initialize_is_dropped() {
    // Every notification except `initialized` / `exit` must be dropped
    // (no handler spawned, no wire output) while uninitialized. `didOpen`
    // is the observable case: its handler would publish a diagnostic, so
    // an empty outbox proves it never ran.
    let cases: &[RawMessage] = &[
        did_open_notification("file:///a"),
        notification("$/cancelRequest", json!({ "id": 1 })),
        notification("$/setTrace", json!({ "value": "verbose" })),
    ];

    for msg in cases {
        let method = match msg {
            RawMessage::Notification { method, .. } => method.clone(),
            _ => unreachable!(),
        };
        let outbox = drive_uninitialized(msg.clone()).await;
        assert!(
            outbox.is_empty(),
            "notification `{method}` before initialize should be dropped, \
             got outbox {outbox:#?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_before_initialize_returns_server_not_initialized() {
    // Every request method except `initialize` must be refused with
    // ServerNotInitialized (-32002) while the server is uninitialized.
    let cases: &[&'static str] = &["shutdown", "textDocument/hover"];

    for method in cases {
        let id = RequestId::Number(1);
        let outbox = drive_uninitialized(request(1, method)).await;
        assert_eq!(
            error_code(&outbox, &id),
            Some(-32002),
            "request `{method}` before initialize should return ServerNotInitialized, \
             got outbox {outbox:#?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_after_shutdown_returns_invalid_request() {
    // After `shutdown` succeeds, every request must be refused with
    // InvalidRequest (-32600). `exit` is a notification and must still work.
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let transport = ChannelTransport {
        in_rx,
        outbox: outbox.clone(),
    };

    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve(Probe, transport).await;
    });

    // Reach Running state.
    in_tx.send(initialize_request(1)).unwrap();
    wait_for_response(&outbox, &RequestId::Number(1), Duration::from_millis(500)).await;

    // Transition to ShuttingDown.
    in_tx.send(request(2, "shutdown")).unwrap();
    wait_for_response(&outbox, &RequestId::Number(2), Duration::from_millis(500)).await;

    // Any request after shutdown is invalid.
    let hover_id = RequestId::Number(3);
    in_tx.send(request(3, "textDocument/hover")).unwrap();
    wait_for_response(&outbox, &hover_id, Duration::from_millis(500)).await;

    // exit notification must still terminate the dispatcher normally.
    in_tx.send(notification("exit", json!(null))).unwrap();

    tokio::time::timeout(Duration::from_secs(2), server_handle)
        .await
        .expect("serve returned within 2s")
        .expect("server task did not panic");

    assert_eq!(
        error_code(&outbox.lock().unwrap(), &hover_id),
        Some(-32600),
        "request after shutdown should return InvalidRequest, got outbox {:#?}",
        outbox.lock().unwrap()
    );
}
