//! ADR 0012: when the in-flight cap is hit, a handler's wait for a permit
//! must be visible in traces as a `handler.acquire_permit` span (issue #3).
//!
//! This lives in its own test binary on purpose. Span capture uses a
//! **process-global** subscriber: a thread-local `set_default` subscriber
//! is not reliably observed when tokio polls spawned handler tasks, so
//! under load spans go uncaptured. A dedicated binary means the global
//! subscriber sees every span this test produces and nothing else's.

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
            // Park until the test signals teardown, so the dispatcher
            // doesn't tear down while handlers are still gated.
            None => {
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

/// A `didOpen` handler gated by explicit barriers rather than a fixed
/// sleep, so the test controls exactly when the permit-holder finishes.
/// Each handler reports it has started (and thus holds the permit) on
/// `started`, then parks until the test releases it.
struct Gated {
    started: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Notify>,
    documents: lspf::Documents,
}

impl LanguageServer for Gated {
    fn documents(&self) -> &lspf::Documents {
        &self.documents
    }

    async fn text_document_did_open(&self, ctx: &Context, params: DidOpenTextDocumentParams) {
        self.started.add_permits(1);
        self.release.notified().await;
        ctx.publish_diagnostics(lspf::types::PublishDiagnosticsParams {
            uri: params.text_document.uri,
            version: Some(params.text_document.version),
            diagnostics: vec![],
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

fn did_open_notification(uri: &str) -> RawMessage {
    let params = json!({
        "textDocument": { "uri": uri, "languageId": "plaintext", "version": 1, "text": "" }
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

/// Captures `handler.acquire_permit` span lifetimes. `on_new_span` stores
/// the open instant in the span's extensions; `on_close` computes the
/// elapsed time — i.e. how long the handler waited for a permit.
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

#[tokio::test(flavor = "current_thread")]
async fn handler_acquire_permit_span_visible_when_cap_exceeded() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let capture = SpanCapture::default();
    tracing_subscriber::registry().with(capture.clone()).init();

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
    // Both `didOpen` handlers are gated on `release`; `started` reports
    // when each one is actually running (and so holds the single permit).
    // Driving the barriers explicitly — rather than racing fixed sleeps —
    // keeps the queueing window deterministic, and waiting for both to
    // publish guarantees every span has closed before we inspect them.
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    let server = Gated {
        started: started.clone(),
        release: release.clone(),
        documents: lspf::Documents::new(),
    };

    const QUEUE_HOLD: Duration = Duration::from_millis(80);
    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve_with_limit(server, transport, 1).await;
    });

    // First handler grabs the only permit and parks; the second is now
    // queued inside `acquire_permit`. Hold it queued for a measurable
    // window so its acquire span shows real wait time, then release the
    // first so the second can acquire (closing its long acquire span).
    let _ = started.acquire().await.unwrap();
    tokio::time::sleep(QUEUE_HOLD).await;
    release.notify_one();

    let _ = started.acquire().await.unwrap();
    release.notify_one();

    // Wait for both handlers to publish before tearing down, so every span
    // has closed. Generous cap guards against a true hang.
    let start = Instant::now();
    while count_publish_diagnostics(&outbox.lock().unwrap()) < 2 {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "handlers did not both publish within 5s"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    done.notify_one();
    let _ = server_handle.await;

    let closed = capture.closed.lock().unwrap();
    let max_wait = closed
        .iter()
        .filter(|(name, _)| name == "handler.acquire_permit")
        .map(|(_, d)| *d)
        .max()
        .unwrap_or_default();

    // The second didOpen was kept queued for `QUEUE_HOLD` behind the
    // first under cap=1, so at least one acquire span must reflect that.
    assert!(
        max_wait >= QUEUE_HOLD / 2,
        "expected an acquire span showing queueing (>= {:?}); spans={:#?}",
        QUEUE_HOLD / 2,
        *closed,
    );
}
