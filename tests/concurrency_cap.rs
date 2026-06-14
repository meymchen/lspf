//! Bounded-concurrency-cap test (issue #3, ADR 0012).
//!
//! Drives the dispatcher with a programmable cap so the test can prove
//! that handlers serialize in batches once the cap is hit. Reuses the
//! `VecTransport` mock shape from `tests/concurrency.rs` — see that file
//! for the rationale.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::json;

use lspf::types::DidOpenTextDocumentParams;
use lspf::{
    Context, LanguageServer, RawMessage, RequestId, Transport, TransportError, TransportReader,
    TransportWriter,
};

struct VecTransport {
    inbox: VecDeque<RawMessage>,
    outbox: Arc<Mutex<Vec<RawMessage>>>,
    done: Arc<tokio::sync::Notify>,
}

struct VecReader {
    inbox: VecDeque<RawMessage>,
    done: Arc<tokio::sync::Notify>,
}

struct VecWriter {
    outbox: Arc<Mutex<Vec<RawMessage>>>,
}

impl Transport for VecTransport {
    type Reader = VecReader;
    type Writer = VecWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            VecReader {
                inbox: self.inbox,
                done: self.done,
            },
            VecWriter {
                outbox: self.outbox,
            },
        )
    }
}

impl TransportReader for VecReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        match self.inbox.pop_front() {
            Some(msg) => Ok(msg),
            None => {
                // Park the read-loop instead of returning `Closed`
                // immediately — otherwise the dispatcher tears down
                // while spawned handlers are still sleeping. The test
                // notifies once it has observed enough output.
                self.done.notified().await;
                Err(TransportError::Closed)
            }
        }
    }
}

impl TransportWriter for VecWriter {
    async fn send(&mut self, msg: RawMessage) -> Result<(), TransportError> {
        self.outbox.lock().unwrap().push(msg);
        Ok(())
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

struct Sleepy {
    sleep: Duration,
    started: Arc<tokio::sync::Semaphore>,
}

impl LanguageServer for Sleepy {
    async fn text_document_did_open(&self, ctx: &Context, params: DidOpenTextDocumentParams) {
        self.started.add_permits(1);
        tokio::time::sleep(self.sleep).await;
        ctx.publish_diagnostics(lspf::types::PublishDiagnosticsParams {
            uri: params.text_document.uri,
            version: Some(params.text_document.version),
            diagnostics: vec![],
        });
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

fn did_open_notification(uri: &str) -> RawMessage {
    let params = json!({
        "textDocument": {
            "uri": uri,
            "languageId": "plaintext",
            "version": 1,
            "text": ""
        }
    });
    RawMessage::Notification {
        method: Cow::Borrowed("textDocument/didOpen"),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn count_publish_diagnostics(outbox: &[RawMessage]) -> usize {
    outbox
        .iter()
        .filter(|m| {
            matches!(
                m,
                RawMessage::Notification { method, .. }
                    if method == "textDocument/publishDiagnostics"
            )
        })
        .count()
}

/// Captures `handler.acquire_permit` span lifetimes. `on_new_span` and
/// `on_close` fire exactly once per span; storing the open instant in
/// the span's extensions lets us compute the elapsed time between
/// acquire-start and acquire-finish — i.e. the queueing latency.
#[derive(Default, Clone)]
struct SpanCapture {
    closed: Arc<Mutex<Vec<(String, Duration)>>>,
}

struct OpenedAt(Instant);

impl<S> tracing_subscriber::Layer<S> for SpanCapture
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        _attrs: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(OpenedAt(Instant::now()));
        }
    }

    fn on_close(&self, id: tracing::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let name = span.metadata().name().to_string();
        let elapsed = span
            .extensions()
            .get::<OpenedAt>()
            .map(|o| o.0.elapsed())
            .unwrap_or_default();
        self.closed.lock().unwrap().push((name, elapsed));
    }
}

// Single-threaded runtime: `set_default()` installs a thread-local
// subscriber, and tokio's current-thread scheduler keeps spawned tasks
// on the same thread so their spans are captured.
#[tokio::test(flavor = "current_thread")]
async fn handler_acquire_permit_span_visible_when_cap_exceeded() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let capture = SpanCapture::default();
    let _guard = tracing_subscriber::registry()
        .with(capture.clone())
        .set_default();

    let outbox = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(tokio::sync::Notify::new());
    let mut inbox = VecDeque::new();
    inbox.push_back(initialize_request(1));
    inbox.push_back(did_open_notification("file:///a"));
    inbox.push_back(did_open_notification("file:///b"));

    let transport = VecTransport {
        inbox,
        outbox: outbox.clone(),
        done: done.clone(),
    };
    let server = Sleepy {
        sleep: Duration::from_millis(150),
        started: Arc::new(tokio::sync::Semaphore::new(0)),
    };

    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve_with_limit(server, transport, 1).await;
    });

    let start = Instant::now();
    while count_publish_diagnostics(&outbox.lock().unwrap()) < 2 {
        if start.elapsed() > Duration::from_millis(1000) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    done.notify_one();
    let _ = server_handle.await;

    let closed = capture.closed.lock().unwrap();
    let acquire_spans: Vec<&(String, Duration)> = closed
        .iter()
        .filter(|(name, _)| name == "handler.acquire_permit")
        .collect();

    // initialize + 2 × didOpen → 3 spawn sites → 3 acquire spans.
    assert_eq!(
        acquire_spans.len(),
        3,
        "expected 3 handler.acquire_permit spans (initialize + 2 didOpen), got {:#?}",
        *closed,
    );
    // First two complete fast; the third queues behind the second didOpen
    // handler's 150ms sleep. Allow a generous lower bound so we don't
    // flake on slow CI but still prove queueing was observed.
    let max_wait = acquire_spans.iter().map(|(_, d)| *d).max().unwrap();
    assert!(
        max_wait >= Duration::from_millis(50),
        "expected at least one acquire span to show queueing (>= 50ms); spans={:#?}",
        acquire_spans,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cap_of_two_serializes_five_handlers_into_three_batches() {
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(tokio::sync::Notify::new());
    let mut inbox = VecDeque::new();
    inbox.push_back(initialize_request(1));
    for i in 0..5 {
        inbox.push_back(did_open_notification(&format!("file:///{i}")));
    }

    let transport = VecTransport {
        inbox,
        outbox: outbox.clone(),
        done: done.clone(),
    };
    let server = Sleepy {
        sleep: Duration::from_millis(200),
        started: Arc::new(tokio::sync::Semaphore::new(0)),
    };

    let start = Instant::now();
    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve_with_limit(server, transport, 2).await;
    });

    // Poll until all five publishDiagnostics land, capped at 1.5s.
    let wall_clock = loop {
        if count_publish_diagnostics(&outbox.lock().unwrap()) >= 5 {
            break start.elapsed();
        }
        if start.elapsed() > Duration::from_millis(1500) {
            break start.elapsed();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    done.notify_one();
    let _ = server_handle.await;

    let final_outbox = outbox.lock().unwrap();
    assert_eq!(
        count_publish_diagnostics(&final_outbox),
        5,
        "expected five publishDiagnostics in outbox, got {}",
        count_publish_diagnostics(&final_outbox),
    );
    // 5 handlers / cap 2 = ceil(5/2) = 3 batches; 3 × 200ms = 600ms floor.
    // Upper bound 800ms leaves 200ms slack for spawn / scheduling jitter.
    assert!(
        wall_clock >= Duration::from_millis(600) && wall_clock <= Duration::from_millis(800),
        "5 × 200ms handlers under cap=2 should take ~600–800ms, took {wall_clock:?}",
    );
}
