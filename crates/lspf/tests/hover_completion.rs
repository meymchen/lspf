//! End-to-end coverage for the 0.2 standard hover and completion features
//! (issue #41).
//!
//! Registering `features::hover()` and `features::completion(options)` supplies
//! both typed dispatch and the minimal capability advertised at initialization.
//! These tests verify the advertised capabilities and the routed results over
//! an in-memory transport, covering the initialize → hover → shutdown flow the
//! issue calls for, with equivalent completion coverage.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use tokio::sync::mpsc;

use lspf::types::{
    CompletionItem, CompletionOptions, CompletionParams, CompletionResponse, Hover, HoverContents,
    HoverParams, MarkedString, ServerCapabilities,
};
use lspf::{
    CancellationToken, Context, LspError, RawMessage, RequestId, Server, Transport, TransportError,
    TransportReader, TransportWriter,
};

/// Application state shared as `Arc<S>` by every handler on the connection.
struct AppState;

async fn hover(
    _state: Arc<AppState>,
    _ctx: Context,
    _params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    Ok(Some(Hover {
        contents: HoverContents::Scalar(MarkedString::String("docs".to_string())),
        range: None,
    }))
}

async fn completion(
    _state: Arc<AppState>,
    _ctx: Context,
    _params: CompletionParams,
    _ct: CancellationToken,
) -> Result<Option<CompletionResponse>, LspError> {
    Ok(Some(CompletionResponse::Array(vec![
        CompletionItem::new_simple("field".to_string(), "a field".to_string()),
    ])))
}

fn completion_options() -> CompletionOptions {
    CompletionOptions {
        trigger_characters: Some(vec![".".to_string()]),
        ..CompletionOptions::default()
    }
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

fn text_document_position(id: i32, method: &'static str) -> RawMessage {
    request(
        id,
        method,
        json!({
            "textDocument": { "uri": "file:///a.rs" },
            "position": { "line": 0, "character": 0 }
        }),
    )
}

fn exit() -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed("exit"),
        params: Bytes::from_static(b"null"),
    }
}

/// Build a server from `register`, feed it `messages`, then close the transport
/// so `serve` returns once everything is processed. Returns the outbox.
async fn drive(messages: Vec<RawMessage>) -> Vec<RawMessage> {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let transport = ChannelTransport { in_rx, out_tx };

    let server = Server::builder(AppState)
        .feature(lspf::features::hover(), hover)
        .feature(lspf::features::completion(completion_options()), completion)
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

// --- Tests -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_hover_shutdown_advertises_and_routes_hover() {
    let outbox = drive(vec![
        initialize_request(1),
        text_document_position(2, "textDocument/hover"),
        request(3, "shutdown", json!(null)),
        exit(),
    ])
    .await;

    // Registering hover sets hover_provider and touches nothing unrelated.
    let init: lspf::types::InitializeResult =
        serde_json::from_value(ok_result(&outbox, 1).expect("initialize response")).unwrap();
    let caps = init.capabilities;
    assert_eq!(
        caps.hover_provider,
        Some(lspf::types::HoverProviderCapability::Simple(true))
    );
    assert_eq!(
        caps.execute_command_provider, None,
        "hover contributes no unrelated capability"
    );

    // The hover request routed to the typed handler and returned its result.
    let hover: Hover =
        serde_json::from_value(ok_result(&outbox, 2).expect("hover response")).unwrap();
    assert_eq!(
        hover.contents,
        HoverContents::Scalar(MarkedString::String("docs".to_string()))
    );

    assert_eq!(ok_result(&outbox, 3), Some(serde_json::Value::Null));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_advertises_supplied_options_and_routes_results() {
    let outbox = drive(vec![
        initialize_request(1),
        text_document_position(2, "textDocument/completion"),
        exit(),
    ])
    .await;

    let init: lspf::types::InitializeResult =
        serde_json::from_value(ok_result(&outbox, 1).expect("initialize response")).unwrap();
    assert_eq!(
        init.capabilities.completion_provider,
        Some(completion_options()),
        "completion advertises exactly the supplied options"
    );

    let completion: CompletionResponse =
        serde_json::from_value(ok_result(&outbox, 2).expect("completion response")).unwrap();
    match completion {
        CompletionResponse::Array(items) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].label, "field");
        }
        other => panic!("expected a completion array, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn features_advertise_no_unrelated_capabilities() {
    let outbox = drive(vec![initialize_request(1), exit()]).await;

    let init: lspf::types::InitializeResult =
        serde_json::from_value(ok_result(&outbox, 1).expect("initialize response")).unwrap();

    // Only hover and completion are set; everything else stays default apart
    // from the protocol-negotiated position encoding (ADR 0016), which defaults
    // to UTF-16 when the client offers none.
    let expected = ServerCapabilities {
        hover_provider: Some(lspf::types::HoverProviderCapability::Simple(true)),
        completion_provider: Some(completion_options()),
        position_encoding: Some(lspf::types::PositionEncodingKind::UTF16),
        ..ServerCapabilities::default()
    };
    assert_eq!(init.capabilities, expected);
}
