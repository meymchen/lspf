//! Document-sync ordering integration tests (issue #9).
//!
//! Drives the dispatcher through an in-process mock [`Transport`] so the
//! test can observe that built-in document mutations land in the
//! [`Documents`] store before the user's notification handler runs.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;

use lspf::types::{
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, Position, PublishDiagnosticsParams,
    Range,
};
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
                // Park the read-loop until the test has observed what it
                // needs; then signal shutdown.
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
        params: Bytes::new(),
    }
}

fn did_save_notification(uri: &str) -> RawMessage {
    let params = json!({
        "textDocument": { "uri": uri }
    });
    RawMessage::Notification {
        method: Cow::Borrowed("textDocument/didSave"),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn did_close_notification(uri: &str) -> RawMessage {
    let params = json!({
        "textDocument": { "uri": uri }
    });
    RawMessage::Notification {
        method: Cow::Borrowed("textDocument/didClose"),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn did_change_notification(
    uri: &str,
    version: i32,
    start: u32,
    end: u32,
    text: &str,
) -> RawMessage {
    let params = json!({
        "textDocument": {
            "uri": uri,
            "version": version
        },
        "contentChanges": [
            {
                "range": {
                    "start": { "line": 0, "character": start },
                    "end": { "line": 0, "character": end }
                },
                "text": text
            }
        ]
    });
    RawMessage::Notification {
        method: Cow::Borrowed("textDocument/didChange"),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn did_open_notification(uri: &str, text: &str) -> RawMessage {
    let params = json!({
        "textDocument": {
            "uri": uri,
            "languageId": "plaintext",
            "version": 1,
            "text": text
        }
    });
    RawMessage::Notification {
        method: Cow::Borrowed("textDocument/didOpen"),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn find_diagnostic_message(outbox: &[RawMessage], want: &str) -> bool {
    outbox.iter().any(|m| {
        let RawMessage::Notification { method, params } = m else {
            return false;
        };
        if method != "textDocument/publishDiagnostics" {
            return false;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(params) else {
            return false;
        };
        value["diagnostics"]
            .as_array()
            .map(|arr| arr.iter().any(|d| d["message"] == want))
            .unwrap_or(false)
    })
}

struct OpenObserver {
    documents: lspf::Documents,
}

impl LanguageServer for OpenObserver {
    fn documents(&self) -> &lspf::Documents {
        &self.documents
    }

    async fn text_document_did_open(&self, ctx: &Context, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let saw_doc = ctx.documents().get(&uri).is_some();
        ctx.publish_diagnostics(PublishDiagnosticsParams {
            uri,
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
                source: Some("doc-sync-test".into()),
                message: if saw_doc {
                    "open-saw-doc".into()
                } else {
                    "open-missing-doc".into()
                },
                ..Diagnostic::default()
            }],
        });
    }
}

struct ChangeObserver {
    documents: lspf::Documents,
}

impl LanguageServer for ChangeObserver {
    fn documents(&self) -> &lspf::Documents {
        &self.documents
    }

    async fn text_document_did_change(&self, ctx: &Context, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let saw_update = ctx
            .documents()
            .get(&uri)
            .map(|doc| doc.text() == "hello lspf" && doc.version() == 2)
            .unwrap_or(false);
        ctx.publish_diagnostics(PublishDiagnosticsParams {
            uri,
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
                source: Some("doc-sync-test".into()),
                message: if saw_update {
                    "change-saw-update".into()
                } else {
                    "change-missing-update".into()
                },
                ..Diagnostic::default()
            }],
        });
    }
}

struct CloseObserver {
    documents: lspf::Documents,
}

impl LanguageServer for CloseObserver {
    fn documents(&self) -> &lspf::Documents {
        &self.documents
    }

    async fn text_document_did_close(&self, ctx: &Context, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let saw_removal = ctx.documents().get(&uri).is_none();
        ctx.publish_diagnostics(PublishDiagnosticsParams {
            uri,
            version: None,
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
                source: Some("doc-sync-test".into()),
                message: if saw_removal {
                    "close-saw-removal".into()
                } else {
                    "close-missing-removal".into()
                },
                ..Diagnostic::default()
            }],
        });
    }
}

struct SaveObserver {
    documents: lspf::Documents,
}

impl LanguageServer for SaveObserver {
    fn documents(&self) -> &lspf::Documents {
        &self.documents
    }

    async fn text_document_did_save(&self, ctx: &Context, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let saw_doc = ctx.documents().get(&uri).is_some();
        ctx.publish_diagnostics(PublishDiagnosticsParams {
            uri,
            version: None,
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
                source: Some("doc-sync-test".into()),
                message: if saw_doc {
                    "save-saw-doc".into()
                } else {
                    "save-missing-doc".into()
                },
                ..Diagnostic::default()
            }],
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn did_save_invokes_handler_after_inline_hook() {
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(tokio::sync::Notify::new());
    let mut inbox = VecDeque::new();
    inbox.push_back(initialize_request(1));
    inbox.push_back(did_open_notification("file:///save.txt", "hello"));
    inbox.push_back(did_save_notification("file:///save.txt"));

    let transport = VecTransport {
        inbox,
        outbox: outbox.clone(),
        done: done.clone(),
    };
    let server = SaveObserver {
        documents: lspf::Documents::new(),
    };

    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve(server, transport).await;
    });

    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "timed out waiting for publishDiagnostics"
        );
        if find_diagnostic_message(&outbox.lock().unwrap(), "save-saw-doc") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    done.notify_one();
    let _ = server_handle.await;

    assert!(
        !find_diagnostic_message(&outbox.lock().unwrap(), "save-missing-doc"),
        "save handler should observe the document that is still open"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn did_close_mutation_lands_before_user_handler_runs() {
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(tokio::sync::Notify::new());
    let mut inbox = VecDeque::new();
    inbox.push_back(initialize_request(1));
    inbox.push_back(did_open_notification("file:///close.txt", "hello"));
    inbox.push_back(did_close_notification("file:///close.txt"));

    let transport = VecTransport {
        inbox,
        outbox: outbox.clone(),
        done: done.clone(),
    };
    let server = CloseObserver {
        documents: lspf::Documents::new(),
    };

    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve(server, transport).await;
    });

    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "timed out waiting for publishDiagnostics"
        );
        if find_diagnostic_message(&outbox.lock().unwrap(), "close-saw-removal") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    done.notify_one();
    let _ = server_handle.await;

    assert!(
        !find_diagnostic_message(&outbox.lock().unwrap(), "close-missing-removal"),
        "handler should observe the removed document after inline close"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn did_change_mutation_lands_before_user_handler_runs() {
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(tokio::sync::Notify::new());
    let mut inbox = VecDeque::new();
    inbox.push_back(initialize_request(1));
    inbox.push_back(did_open_notification("file:///change.txt", "hello world"));
    inbox.push_back(did_change_notification(
        "file:///change.txt",
        2,
        6,
        11,
        "lspf",
    ));

    let transport = VecTransport {
        inbox,
        outbox: outbox.clone(),
        done: done.clone(),
    };
    let server = ChangeObserver {
        documents: lspf::Documents::new(),
    };

    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve(server, transport).await;
    });

    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "timed out waiting for publishDiagnostics"
        );
        if find_diagnostic_message(&outbox.lock().unwrap(), "change-saw-update") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    done.notify_one();
    let _ = server_handle.await;

    assert!(
        !find_diagnostic_message(&outbox.lock().unwrap(), "change-missing-update"),
        "handler should observe the updated document after inline change"
    );
}

struct OrderingCheck {
    documents: lspf::Documents,
    uri: String,
}

impl LanguageServer for OrderingCheck {
    fn documents(&self) -> &lspf::Documents {
        &self.documents
    }

    async fn shutdown(
        &self,
        ctx: &Context,
        _ct: lspf::CancellationToken,
    ) -> Result<(), lspf::LspError> {
        let uri = lspf::types::Uri::from_str(&self.uri).unwrap();
        let text = ctx
            .documents()
            .get(&uri)
            .map(|doc| doc.text())
            .unwrap_or_default();
        ctx.publish_diagnostics(PublishDiagnosticsParams {
            uri,
            version: None,
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
                source: Some("doc-sync-test".into()),
                message: format!("shutdown-sees-{text}"),
                ..Diagnostic::default()
            }],
        });
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn did_change_mutation_is_visible_to_following_request() {
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(tokio::sync::Notify::new());
    let mut inbox = VecDeque::new();
    inbox.push_back(initialize_request(1));
    inbox.push_back(did_open_notification("file:///ordering.txt", "hello world"));
    inbox.push_back(did_change_notification(
        "file:///ordering.txt",
        2,
        6,
        11,
        "lspf",
    ));
    inbox.push_back(shutdown_request(2));

    let transport = VecTransport {
        inbox,
        outbox: outbox.clone(),
        done: done.clone(),
    };
    let server = OrderingCheck {
        documents: lspf::Documents::new(),
        uri: "file:///ordering.txt".to_string(),
    };

    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve(server, transport).await;
    });

    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "timed out waiting for shutdown diagnostic"
        );
        if find_diagnostic_message(&outbox.lock().unwrap(), "shutdown-sees-hello lspf") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    done.notify_one();
    let _ = server_handle.await;

    assert!(
        !find_diagnostic_message(&outbox.lock().unwrap(), "shutdown-sees-hello world"),
        "following request should see the post-change document, not the original"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn did_open_mutation_lands_before_user_handler_runs() {
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(tokio::sync::Notify::new());
    let mut inbox = VecDeque::new();
    inbox.push_back(initialize_request(1));
    inbox.push_back(did_open_notification("file:///open.txt", "hello"));

    let transport = VecTransport {
        inbox,
        outbox: outbox.clone(),
        done: done.clone(),
    };
    let server = OpenObserver {
        documents: lspf::Documents::new(),
    };

    let server_handle = tokio::spawn(async move {
        let _ = lspf::serve(server, transport).await;
    });

    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "timed out waiting for publishDiagnostics"
        );
        if find_diagnostic_message(&outbox.lock().unwrap(), "open-saw-doc") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    done.notify_one();
    let _ = server_handle.await;

    assert!(
        !find_diagnostic_message(&outbox.lock().unwrap(), "open-missing-doc"),
        "handler should never observe a missing document after inline open"
    );
}
