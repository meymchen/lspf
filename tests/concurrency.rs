//! Concurrent-dispatch integration test (issue #1).
//!
//! Drives the dispatcher directly through an in-process mock
//! [`Transport`] so handler interleaving and timing can be observed
//! without subprocess + stdio framing noise. See ADR 0015 for why the
//! mock just splits a `VecDeque` (inbound) from an `Arc<Mutex<Vec<_>>>`
//! (outbound).

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::json;

use lspf::types::{
    Diagnostic, DiagnosticSeverity, DidOpenTextDocumentParams, Position, PublishDiagnosticsParams,
    Range,
};
use lspf::{
    Context, LanguageServer, RawMessage, RequestId, Transport, TransportError, TransportReader,
    TransportWriter,
};

struct VecTransport {
    inbox: VecDeque<RawMessage>,
    outbox: Arc<Mutex<Vec<RawMessage>>>,
}

struct VecReader {
    inbox: VecDeque<RawMessage>,
}

struct VecWriter {
    outbox: Arc<Mutex<Vec<RawMessage>>>,
}

impl Transport for VecTransport {
    type Reader = VecReader;
    type Writer = VecWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            VecReader { inbox: self.inbox },
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
            None => Err(TransportError::Closed),
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

struct Sleepy;

const HANDLER_SLEEP: Duration = Duration::from_millis(500);

impl LanguageServer for Sleepy {
    async fn text_document_did_open(&self, ctx: &Context, params: DidOpenTextDocumentParams) {
        tokio::time::sleep(HANDLER_SLEEP).await;
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
                source: Some("concurrency-test".into()),
                message: "sleepy".into(),
                ..Diagnostic::default()
            }],
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
            "text": "hello"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_did_open_handlers_run_concurrently() {
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let mut inbox = VecDeque::new();
    inbox.push_back(initialize_request(1));
    inbox.push_back(did_open_notification("file:///a"));
    inbox.push_back(did_open_notification("file:///b"));

    let transport = VecTransport {
        inbox,
        outbox: outbox.clone(),
    };

    let start = Instant::now();
    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve(Sleepy, transport).await;
    });

    // Poll until both publishDiagnostics land, capped at 2s.
    let wall_clock = loop {
        if count_publish_diagnostics(&outbox.lock().unwrap()) >= 2 {
            break start.elapsed();
        }
        if start.elapsed() > Duration::from_secs(2) {
            break start.elapsed();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    let _ = server_handle.await;

    let final_outbox = outbox.lock().unwrap();
    assert_eq!(
        count_publish_diagnostics(&final_outbox),
        2,
        "expected two publishDiagnostics in outbox, got {:#?}",
        *final_outbox
    );
    assert!(
        wall_clock < Duration::from_millis(700),
        "two 500ms didOpen handlers should run concurrently in < 700ms, took {:?}",
        wall_clock
    );
}
