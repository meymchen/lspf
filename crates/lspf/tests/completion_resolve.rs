//! End-to-end coverage for the completion capability family (issue #69).
//!
//! Registering `features::completion(options)` and
//! `features::completion_resolve()` routes both `textDocument/completion` and
//! `completionItem/resolve` with typed values, while one family-aware merge
//! emits a single deterministic `completionProvider` capability — verified
//! byte-for-byte against `fixtures/completion_provider_with_resolve.json`.
//! Static and initialize-conditional registrations share the same merge and
//! validation rules, so a resolve registered without its base fails the
//! initialize transaction exactly as a static one fails `build`.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use tokio::sync::mpsc;

use lspf::types::{CompletionItem, CompletionOptions, CompletionParams, CompletionResponse};
use lspf::{
    CancellationToken, Context, LspError, RawMessage, RequestId, Server, Transport, TransportError,
    TransportReader, TransportWriter,
};

/// Application state shared as `Arc<S>` by every handler on the connection.
struct AppState;

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

async fn resolve(
    _state: Arc<AppState>,
    _ctx: Context,
    mut item: CompletionItem,
    _ct: CancellationToken,
) -> Result<CompletionItem, LspError> {
    item.detail = Some("resolved detail".to_string());
    Ok(item)
}

fn completion_options() -> CompletionOptions {
    CompletionOptions {
        trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
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

fn exit() -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed("exit"),
        params: Bytes::from_static(b"null"),
    }
}

/// Drive `server` with `messages`, then close the transport so `serve` returns
/// once everything is processed. Returns the outbox.
async fn drive(server: Server<AppState>, messages: Vec<RawMessage>) -> Vec<RawMessage> {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let transport = ChannelTransport { in_rx, out_tx };

    let mut handle = tokio::spawn(async move { server.serve(transport).await });
    let mut server_done = false;
    let mut outbox = Vec::new();

    'messages: for msg in messages {
        let response_id = msg.id().cloned();
        // A failed initialize transaction terminates the connection, so a send
        // can legitimately race the disconnect; stop feeding the channel
        // instead of panicking on `SendError`.
        if in_tx.send(msg).is_err() {
            break;
        }
        if let Some(response_id) = response_id {
            tokio::select! {
                response = out_rx.recv() => {
                    if let Some(response) = response {
                        assert_eq!(response.id(), Some(&response_id));
                        outbox.push(response);
                    } else {
                        (&mut handle)
                            .await
                            .expect("server task did not panic")
                            .expect("serve ended cleanly");
                        server_done = true;
                        break 'messages;
                    }
                }
                result = &mut handle => {
                    result
                        .expect("server task did not panic")
                        .expect("serve ended cleanly");
                    server_done = true;
                    break 'messages;
                }
            }
        }
    }
    drop(in_tx);

    if !server_done {
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("serve returned within 2s")
            .expect("server task did not panic")
            .expect("serve ended cleanly");
    }

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

fn completion_provider(outbox: &[RawMessage], id: i32) -> CompletionOptions {
    let init: lspf::types::InitializeResult =
        serde_json::from_value(ok_result(outbox, id).expect("initialize response")).unwrap();
    init.capabilities
        .completion_provider
        .expect("the family advertises one completionProvider capability")
}

// --- Tests -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_and_resolve_dispatch_typed_values_and_merge_one_capability() {
    let server = Server::builder(AppState)
        .feature(lspf::features::completion(completion_options()), completion)
        .feature(lspf::features::completion_resolve(), resolve)
        .build()
        .expect("completion and resolve build");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            request(
                2,
                "textDocument/completion",
                json!({
                    "textDocument": { "uri": "file:///a.rs" },
                    "position": { "line": 0, "character": 0 }
                }),
            ),
            request(3, "completionItem/resolve", json!({ "label": "field" })),
            exit(),
        ],
    )
    .await;

    // The family merge emits one capability carrying both contributions.
    let merged = completion_provider(&outbox, 1);
    assert_eq!(merged.resolve_provider, Some(true));
    assert_eq!(
        merged.trigger_characters,
        Some(vec![".".to_string(), ":".to_string()])
    );

    // Both routes dispatch typed values.
    let completion: CompletionResponse =
        serde_json::from_value(ok_result(&outbox, 2).expect("completion response")).unwrap();
    match completion {
        CompletionResponse::Array(items) => assert_eq!(items[0].label, "field"),
        other => panic!("expected a completion array, got {other:?}"),
    }
    let resolved: CompletionItem =
        serde_json::from_value(ok_result(&outbox, 3).expect("resolve response")).unwrap();
    assert_eq!(resolved.label, "field");
    assert_eq!(resolved.detail.as_deref(), Some("resolved detail"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_resolve_uses_the_same_merge() {
    let server = Server::builder(AppState)
        .feature(lspf::features::completion(completion_options()), completion)
        .configure_initialize(|_params, registrar| {
            registrar.feature(lspf::features::completion_resolve(), resolve);
            Ok(())
        })
        .build()
        .expect("a static base with a conditional resolve builds");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;

    let merged = completion_provider(&outbox, 1);
    assert_eq!(
        merged.resolve_provider,
        Some(true),
        "conditional registrations merge through the same family rules"
    );
    assert_eq!(
        merged.trigger_characters,
        Some(vec![".".to_string(), ":".to_string()])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_resolve_without_completion_fails_initialization() {
    let server = Server::builder(AppState)
        .configure_initialize(|_params, registrar| {
            registrar.feature(lspf::features::completion_resolve(), resolve);
            Ok(())
        })
        .build()
        .expect("build does not run the conditional transaction");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;

    assert_eq!(
        error_code(&outbox, 1),
        Some(-32603),
        "combined validation rejects the dangling resolve with InternalError"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merged_completion_provider_is_byte_stable() {
    let server = Server::builder(AppState)
        .feature(lspf::features::completion(completion_options()), completion)
        .feature(lspf::features::completion_resolve(), resolve)
        .build()
        .expect("completion and resolve build");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;

    // Compare against the raw wire bytes, not a re-serialized typed value, so
    // an added, renamed, or reordered field in the emitted object breaks here.
    let wire = match response(&outbox, 1).expect("initialize response") {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => String::from_utf8(bytes.to_vec()).unwrap(),
        other => panic!("expected a successful initialize response, got {other:?}"),
    };
    let fixture = include_str!("fixtures/completion_provider_with_resolve.json").trim_end();
    assert!(
        wire.contains(&format!("\"completionProvider\":{fixture}")),
        "the merged completionProvider on the wire must stay byte-stable; \
         update the fixture only with a deliberate capability change.\nwire: {wire}"
    );
}
