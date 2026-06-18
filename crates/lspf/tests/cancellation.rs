//! Request-level cancellation (issue #2, ADR 0007).
//!
//! Drives the dispatcher through a channel-backed mock [`Transport`] so
//! the test can push `$/cancelRequest` after a long-running request is
//! already in flight. Mirrors `concurrency.rs` in shape but swaps the
//! `VecDeque` inbox for an `mpsc::UnboundedReceiver` to allow
//! out-of-band injection.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use lspf::{
    CancellationToken, Context, LanguageServer, LspError, RawMessage, RequestId, Transport,
    TransportError, TransportReader, TransportWriter,
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

fn initialize_request(id: i32) -> RawMessage {
    let params = json!({
        "processId": null,
        "rootUri": null,
        "capabilities": {}
    });
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed("initialize"),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn shutdown_request(id: i32) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed("shutdown"),
        params: Bytes::from_static(b"{}"),
    }
}

fn cancel_notification(id: i32) -> RawMessage {
    let params = json!({ "id": id });
    RawMessage::Notification {
        method: Cow::Borrowed("$/cancelRequest"),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn find_response(outbox: &[RawMessage], target: &RequestId) -> Option<RawMessage> {
    outbox
        .iter()
        .find(|m| matches!(m, RawMessage::Response { id, .. } if id == target))
        .cloned()
}

async fn poll_for_response(
    outbox: &Arc<Mutex<Vec<RawMessage>>>,
    id: &RequestId,
    deadline: Duration,
) -> Option<RawMessage> {
    let start = Instant::now();
    loop {
        if let Some(resp) = find_response(&outbox.lock().unwrap(), id) {
            return Some(resp);
        }
        if start.elapsed() > deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Drive `initialize` to completion so the server reaches `Running` —
/// the only state in which the LSP spec permits a client to send (and
/// therefore cancel) further requests. Panics if the initialize response
/// does not arrive promptly.
async fn initialize(
    in_tx: &mpsc::UnboundedSender<RawMessage>,
    outbox: &Arc<Mutex<Vec<RawMessage>>>,
) {
    in_tx.send(initialize_request(1)).unwrap();
    poll_for_response(outbox, &RequestId::Number(1), Duration::from_millis(500))
        .await
        .expect("initialize did not complete within 500ms");
}

/// A server whose `shutdown` sleeps for a long time, bailing politely
/// when the framework triggers its cancellation token. Cancellation is
/// exercised on `shutdown` (a post-initialize request) rather than
/// `initialize`, because the spec forbids clients from sending anything —
/// including `$/cancelRequest` — before the initialize response, and the
/// dispatcher drops such notifications (issue #4).
struct SleepyShutdown {
    documents: lspf::Documents,
}

impl LanguageServer for SleepyShutdown {
    fn documents(&self) -> &lspf::Documents {
        &self.documents
    }

    async fn shutdown(&self, _ctx: &Context, ct: CancellationToken) -> Result<(), LspError> {
        tokio::select! {
            _ = ct.cancelled() => Err(LspError::RequestCancelled),
            _ = tokio::time::sleep(Duration::from_secs(1)) => Ok(()),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_request_returns_request_cancelled() {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let outbox = Arc::new(Mutex::new(Vec::new()));

    let transport = ChannelTransport {
        in_rx,
        outbox: outbox.clone(),
    };
    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve(
            SleepyShutdown {
                documents: lspf::Documents::new(),
            },
            transport,
        )
        .await;
    });

    initialize(&in_tx, &outbox).await;

    in_tx.send(shutdown_request(2)).unwrap();
    // Give the spawned handler a moment to land in its await before the cancel.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let cancel_sent = Instant::now();
    in_tx.send(cancel_notification(2)).unwrap();

    let response = poll_for_response(&outbox, &RequestId::Number(2), Duration::from_millis(500))
        .await
        .expect("no response for id=2 within 500ms");
    let elapsed = cancel_sent.elapsed();

    assert!(
        elapsed < Duration::from_millis(200),
        "cancel→response took {elapsed:?}, expected < 200ms"
    );

    match response {
        RawMessage::Response {
            result: Err(err), ..
        } => {
            assert_eq!(
                err.code, -32800,
                "expected RequestCancelled code, got {err:?}"
            );
        }
        other => panic!("expected error response, got {other:?}"),
    }

    drop(in_tx);
    let _ = server_handle.await;
}

/// A server whose `shutdown` parks on `ct.cancelled()` then asserts the
/// token observed the cancel, signalling via a oneshot.
struct ObserveCancel {
    signal: Mutex<Option<oneshot::Sender<bool>>>,
    documents: lspf::Documents,
}

impl LanguageServer for ObserveCancel {
    fn documents(&self) -> &lspf::Documents {
        &self.documents
    }

    async fn shutdown(&self, _ctx: &Context, ct: CancellationToken) -> Result<(), LspError> {
        ct.cancelled().await;
        let observed = ct.is_cancelled();
        if let Some(tx) = self.signal.lock().unwrap().take() {
            let _ = tx.send(observed);
        }
        Err(LspError::RequestCancelled)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_request_triggers_handler_token() {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let (signal_tx, signal_rx) = oneshot::channel::<bool>();

    let server = ObserveCancel {
        signal: Mutex::new(Some(signal_tx)),
        documents: lspf::Documents::new(),
    };
    let transport = ChannelTransport {
        in_rx,
        outbox: outbox.clone(),
    };
    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve(server, transport).await;
    });

    initialize(&in_tx, &outbox).await;

    in_tx.send(shutdown_request(2)).unwrap();
    // Ensure the spawned handler reaches `ct.cancelled().await` before cancel arrives.
    tokio::time::sleep(Duration::from_millis(20)).await;
    in_tx.send(cancel_notification(2)).unwrap();

    let observed = tokio::time::timeout(Duration::from_millis(100), signal_rx)
        .await
        .expect("handler did not observe cancellation within 100ms")
        .expect("handler dropped signal sender");
    assert!(observed, "ct.is_cancelled() returned false in handler");

    drop(in_tx);
    let _ = server_handle.await;
}
