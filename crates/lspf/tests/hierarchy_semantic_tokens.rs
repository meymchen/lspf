//! End-to-end coverage for hierarchy and semantic-token feature families.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use lspf::types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOptions, CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams,
    CallHierarchyPrepareParams, Position, Range, SemanticToken, SemanticTokenType, SemanticTokens,
    SemanticTokensDelta, SemanticTokensDeltaParams, SemanticTokensEdit,
    SemanticTokensFullDeltaResult, SemanticTokensLegend, SemanticTokensOptions,
    SemanticTokensParams, SemanticTokensRangeParams, SemanticTokensRangeResult,
    SemanticTokensResult, SymbolKind, TypeHierarchyItem, TypeHierarchyOptions,
    TypeHierarchyPrepareParams, TypeHierarchySubtypesParams, TypeHierarchySupertypesParams,
};
use lspf::{
    BuildError, CancellationToken, LspError, RawMessage, RequestId, Server, ServerContext,
    Transport, TransportError, TransportReader, TransportWriter,
};

struct AppState;

fn call_item(name: &str) -> CallHierarchyItem {
    CallHierarchyItem {
        name: name.to_string(),
        kind: SymbolKind::Function,
        tags: None,
        detail: None,
        uri: "file:///a.rs".parse().unwrap(),
        range: Range::new(Position::new(0, 0), Position::new(0, 4)),
        selection_range: Range::new(Position::new(0, 0), Position::new(0, 4)),
        data: None,
    }
}

fn type_item(name: &str) -> TypeHierarchyItem {
    TypeHierarchyItem {
        name: name.to_string(),
        kind: SymbolKind::Class,
        tags: None,
        detail: None,
        uri: "file:///a.rs".parse().unwrap(),
        range: Range::new(Position::new(1, 0), Position::new(1, 4)),
        selection_range: Range::new(Position::new(1, 0), Position::new(1, 4)),
        data: None,
    }
}

async fn call_prepare(
    _: Arc<AppState>,
    _: ServerContext,
    _: CallHierarchyPrepareParams,
    _: CancellationToken,
) -> Result<Option<Vec<CallHierarchyItem>>, LspError> {
    Ok(Some(vec![call_item("prepared")]))
}

async fn incoming_calls(
    _: Arc<AppState>,
    _: ServerContext,
    _: CallHierarchyIncomingCallsParams,
    _: CancellationToken,
) -> Result<Option<Vec<CallHierarchyIncomingCall>>, LspError> {
    Ok(Some(vec![CallHierarchyIncomingCall {
        from: call_item("incoming"),
        from_ranges: vec![],
    }]))
}

async fn outgoing_calls(
    _: Arc<AppState>,
    _: ServerContext,
    _: CallHierarchyOutgoingCallsParams,
    _: CancellationToken,
) -> Result<Option<Vec<CallHierarchyOutgoingCall>>, LspError> {
    Ok(Some(vec![CallHierarchyOutgoingCall {
        to: call_item("outgoing"),
        from_ranges: vec![],
    }]))
}

async fn type_prepare(
    _: Arc<AppState>,
    _: ServerContext,
    _: TypeHierarchyPrepareParams,
    _: CancellationToken,
) -> Result<Option<Vec<TypeHierarchyItem>>, LspError> {
    Ok(Some(vec![type_item("prepared type")]))
}

async fn supertypes(
    _: Arc<AppState>,
    _: ServerContext,
    _: TypeHierarchySupertypesParams,
    _: CancellationToken,
) -> Result<Option<Vec<TypeHierarchyItem>>, LspError> {
    Ok(Some(vec![type_item("supertype")]))
}

async fn subtypes(
    _: Arc<AppState>,
    _: ServerContext,
    _: TypeHierarchySubtypesParams,
    _: CancellationToken,
) -> Result<Option<Vec<TypeHierarchyItem>>, LspError> {
    Ok(Some(vec![type_item("subtype")]))
}

fn tokens(result_id: &str) -> SemanticTokens {
    SemanticTokens {
        result_id: Some(result_id.to_string()),
        data: vec![SemanticToken {
            delta_line: 0,
            delta_start: 0,
            length: 4,
            token_type: 0,
            token_modifiers_bitset: 0,
        }],
    }
}

async fn semantic_full(
    _: Arc<AppState>,
    _: ServerContext,
    _: SemanticTokensParams,
    _: CancellationToken,
) -> Result<Option<SemanticTokensResult>, LspError> {
    Ok(Some(tokens("full").into()))
}

async fn semantic_delta(
    _: Arc<AppState>,
    _: ServerContext,
    _: SemanticTokensDeltaParams,
    _: CancellationToken,
) -> Result<Option<SemanticTokensFullDeltaResult>, LspError> {
    Ok(Some(
        SemanticTokensDelta {
            result_id: Some("delta".to_string()),
            edits: vec![SemanticTokensEdit {
                start: 0,
                delete_count: 0,
                data: None,
            }],
        }
        .into(),
    ))
}

async fn semantic_range(
    _: Arc<AppState>,
    _: ServerContext,
    _: SemanticTokensRangeParams,
    _: CancellationToken,
) -> Result<Option<SemanticTokensRangeResult>, LspError> {
    Ok(Some(tokens("range").into()))
}

fn hierarchy_options() -> lspf::types::WorkDoneProgressOptions {
    lspf::types::WorkDoneProgressOptions {
        work_done_progress: Some(true),
    }
}

fn semantic_options() -> SemanticTokensOptions {
    SemanticTokensOptions {
        work_done_progress_options: hierarchy_options(),
        legend: SemanticTokensLegend {
            token_types: vec![SemanticTokenType::Keyword.to_string()],
            token_modifiers: vec![],
        },
        range: None,
        full: None,
    }
}

fn server() -> Server<AppState> {
    Server::builder(AppState)
        .feature(
            lspf::features::call_hierarchy_prepare(CallHierarchyOptions {
                work_done_progress_options: hierarchy_options(),
            }),
            call_prepare,
        )
        .feature(
            lspf::features::call_hierarchy_incoming_calls(),
            incoming_calls,
        )
        .feature(
            lspf::features::call_hierarchy_outgoing_calls(),
            outgoing_calls,
        )
        .feature(
            lspf::features::type_hierarchy_prepare(TypeHierarchyOptions {
                work_done_progress_options: hierarchy_options(),
            }),
            type_prepare,
        )
        .feature(lspf::features::type_hierarchy_supertypes(), supertypes)
        .feature(lspf::features::type_hierarchy_subtypes(), subtypes)
        .feature(
            lspf::features::semantic_tokens_full(semantic_options()),
            semantic_full,
        )
        .feature(
            lspf::features::semantic_tokens_full_delta(semantic_options()),
            semantic_delta,
        )
        .feature(
            lspf::features::semantic_tokens_range(semantic_options()),
            semantic_range,
        )
        .build()
        .expect("complete families build")
}

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

fn request(id: i32, method: &'static str, params: Value) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn initialize_request() -> RawMessage {
    request(
        1,
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

async fn drive(server: Server<AppState>, messages: Vec<RawMessage>) -> Vec<RawMessage> {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let transport = ChannelTransport { in_rx, out_tx };
    let mut handle = tokio::spawn(async move { server.serve(transport).await });
    let mut server_done = false;
    let mut outbox = Vec::new();

    'messages: for message in messages {
        let response_id = message.id().cloned();
        if in_tx.send(message).is_err() {
            break;
        }
        if let Some(response_id) = response_id {
            tokio::select! {
                response = out_rx.recv() => {
                    let Some(response) = response else {
                        server_done = true;
                        break 'messages;
                    };
                    assert_eq!(response.id(), Some(&response_id));
                    outbox.push(response);
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
            .expect("serve returned")
            .expect("server task did not panic")
            .expect("serve ended cleanly");
    }

    outbox.extend(std::iter::from_fn(|| out_rx.try_recv().ok()));
    outbox
}

fn result(outbox: &[RawMessage], id: i32) -> Value {
    let response = outbox
        .iter()
        .find(|message| message.id() == Some(&RequestId::Number(id)))
        .expect("response id");
    let RawMessage::Response {
        result: Ok(bytes), ..
    } = response
    else {
        panic!("successful response")
    };
    serde_json::from_slice(bytes).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_family_methods_dispatch_independently_and_emit_one_capability_each() {
    let call = serde_json::to_value(call_item("input")).unwrap();
    let ty = serde_json::to_value(type_item("input type")).unwrap();
    let outbox = drive(
        server(),
        vec![
            initialize_request(),
            request(2, "textDocument/prepareCallHierarchy", json!({
                "textDocument": { "uri": "file:///a.rs" }, "position": { "line": 0, "character": 0 }
            })),
            request(3, "callHierarchy/incomingCalls", json!({ "item": call })),
            request(4, "callHierarchy/outgoingCalls", json!({ "item": call_item("input") })),
            request(5, "textDocument/prepareTypeHierarchy", json!({
                "textDocument": { "uri": "file:///a.rs" }, "position": { "line": 1, "character": 0 }
            })),
            request(6, "typeHierarchy/supertypes", json!({ "item": ty })),
            request(7, "typeHierarchy/subtypes", json!({ "item": type_item("input type") })),
            request(8, "textDocument/semanticTokens/full", json!({
                "textDocument": { "uri": "file:///a.rs" }
            })),
            request(9, "textDocument/semanticTokens/full/delta", json!({
                "textDocument": { "uri": "file:///a.rs" }, "previousResultId": "full"
            })),
            request(10, "textDocument/semanticTokens/range", json!({
                "textDocument": { "uri": "file:///a.rs" },
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 1, "character": 0 } }
            })),
            exit(),
        ],
    )
    .await;

    let capabilities = &result(&outbox, 1)["capabilities"];
    assert_eq!(
        capabilities["callHierarchyProvider"]["workDoneProgress"],
        true
    );
    assert_eq!(
        capabilities["typeHierarchyProvider"]["workDoneProgress"],
        true
    );
    assert_eq!(capabilities["semanticTokensProvider"]["range"], true);
    assert_eq!(
        capabilities["semanticTokensProvider"]["full"]["delta"],
        true
    );

    assert_eq!(result(&outbox, 2)[0]["name"], "prepared");
    assert_eq!(result(&outbox, 3)[0]["from"]["name"], "incoming");
    assert_eq!(result(&outbox, 4)[0]["to"]["name"], "outgoing");
    assert_eq!(result(&outbox, 5)[0]["name"], "prepared type");
    assert_eq!(result(&outbox, 6)[0]["name"], "supertype");
    assert_eq!(result(&outbox, 7)[0]["name"], "subtype");
    assert_eq!(result(&outbox, 8)["resultId"], "full");
    assert_eq!(result(&outbox, 9)["resultId"], "delta");
    assert_eq!(result(&outbox, 10)["resultId"], "range");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn combined_semantic_token_provider_is_byte_stable() {
    let outbox = drive(server(), vec![initialize_request(), exit()]).await;
    let response = outbox
        .iter()
        .find(|message| message.id() == Some(&RequestId::Number(1)))
        .expect("initialize response");
    let RawMessage::Response {
        result: Ok(bytes), ..
    } = response
    else {
        panic!("successful initialize response")
    };
    let wire = String::from_utf8(bytes.to_vec()).unwrap();
    let fixture = include_str!("fixtures/semantic_tokens_full_delta_range.json").trim_end();
    assert!(
        wire.contains(&format!("\"semanticTokensProvider\":{fixture}")),
        "semanticTokensProvider must stay byte-stable; wire: {wire}"
    );
}

#[test]
fn missing_primary_methods_fail_with_capability_conflict() {
    let call_err = Server::builder(AppState)
        .feature(
            lspf::features::call_hierarchy_incoming_calls(),
            incoming_calls,
        )
        .build()
        .err()
        .expect("incoming calls need prepare");
    assert_eq!(
        call_err,
        BuildError::ConflictingCapability {
            field: "callHierarchyProvider"
        }
    );

    let type_err = Server::builder(AppState)
        .feature(lspf::features::type_hierarchy_subtypes(), subtypes)
        .build()
        .err()
        .expect("subtypes need prepare");
    assert_eq!(
        type_err,
        BuildError::ConflictingCapability {
            field: "typeHierarchyProvider"
        }
    );

    let semantic_err = Server::builder(AppState)
        .feature(
            lspf::features::semantic_tokens_full_delta(semantic_options()),
            semantic_delta,
        )
        .build()
        .err()
        .expect("delta needs full");
    assert_eq!(
        semantic_err,
        BuildError::ConflictingCapability {
            field: "semanticTokensProvider"
        }
    );
}
