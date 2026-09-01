//! End-to-end coverage for the navigation and lookup feature descriptors.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use lspf::types::request::{
    GotoDeclarationParams, GotoDeclarationResponse, GotoImplementationParams,
    GotoImplementationResponse, GotoTypeDefinitionParams, GotoTypeDefinitionResponse,
};
use lspf::types::{
    BaseSymbolInformation, DeclarationOptions, DefinitionOptions, DocumentHighlight,
    DocumentHighlightKind, DocumentHighlightOptions, DocumentHighlightParams,
    DocumentSymbolOptions, DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams,
    GotoDefinitionResponse, ImplementationRegistrationOptions, LinkedEditingRangeOptions,
    LinkedEditingRangeParams, LinkedEditingRanges, Location, Moniker, MonikerKind, MonikerOptions,
    MonikerParams, Position, Range, ReferenceParams, ReferencesOptions, SignatureHelp,
    SignatureHelpOptions, SignatureHelpParams, SignatureInformation, StaticRegistrationOptions,
    SymbolInformation, SymbolKind, TypeDefinitionRegistrationOptions, UniquenessLevel,
};
use lspf::{
    BuildError, CancellationToken, LspError, RawMessage, RequestId, Server, ServerContext,
    Transport, TransportError, TransportReader, TransportWriter,
};

struct AppState;

fn location(uri: &str, line: u32) -> Location {
    Location {
        uri: uri.parse().unwrap(),
        range: Range::new(Position::new(line, 0), Position::new(line, 4)),
    }
}

async fn signature_help(
    _: Arc<AppState>,
    _: ServerContext,
    _: SignatureHelpParams,
    _: CancellationToken,
) -> Result<Option<SignatureHelp>, LspError> {
    Ok(Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: "fn main()".to_string(),
            documentation: None,
            parameters: None,
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: None,
    }))
}

async fn declaration(
    _: Arc<AppState>,
    _: ServerContext,
    _: GotoDeclarationParams,
    _: CancellationToken,
) -> Result<Option<GotoDeclarationResponse>, LspError> {
    Ok(Some(GotoDeclarationResponse::Declaration(
        location("file:///decl.rs", 2).into(),
    )))
}

async fn definition(
    _: Arc<AppState>,
    _: ServerContext,
    _: GotoDefinitionParams,
    _: CancellationToken,
) -> Result<Option<GotoDefinitionResponse>, LspError> {
    Ok(Some(GotoDefinitionResponse::Definition(
        location("file:///def.rs", 3).into(),
    )))
}

async fn type_definition(
    _: Arc<AppState>,
    _: ServerContext,
    _: GotoTypeDefinitionParams,
    _: CancellationToken,
) -> Result<Option<GotoTypeDefinitionResponse>, LspError> {
    Ok(Some(GotoTypeDefinitionResponse::Definition(
        location("file:///type.rs", 4).into(),
    )))
}

async fn implementation(
    _: Arc<AppState>,
    _: ServerContext,
    _: GotoImplementationParams,
    _: CancellationToken,
) -> Result<Option<GotoImplementationResponse>, LspError> {
    Ok(Some(GotoImplementationResponse::Definition(
        location("file:///impl.rs", 5).into(),
    )))
}

async fn references(
    _: Arc<AppState>,
    _: ServerContext,
    _: ReferenceParams,
    _: CancellationToken,
) -> Result<Option<Vec<Location>>, LspError> {
    Ok(Some(vec![location("file:///use.rs", 6)]))
}

async fn document_highlight(
    _: Arc<AppState>,
    _: ServerContext,
    _: DocumentHighlightParams,
    _: CancellationToken,
) -> Result<Option<Vec<DocumentHighlight>>, LspError> {
    Ok(Some(vec![DocumentHighlight {
        range: Range::new(Position::new(7, 0), Position::new(7, 4)),
        kind: Some(DocumentHighlightKind::Read),
    }]))
}

#[allow(deprecated)]
async fn document_symbol(
    _: Arc<AppState>,
    _: ServerContext,
    _: DocumentSymbolParams,
    _: CancellationToken,
) -> Result<Option<DocumentSymbolResponse>, LspError> {
    Ok(Some(DocumentSymbolResponse::SymbolInformationList(vec![
        SymbolInformation {
            deprecated: None,
            location: location("file:///symbols.rs", 8),
            base_symbol_information: BaseSymbolInformation {
                name: "main".to_string(),
                kind: SymbolKind::Function,
                tags: None,
                container_name: None,
            },
        },
    ])))
}

async fn linked_editing_range(
    _: Arc<AppState>,
    _: ServerContext,
    _: LinkedEditingRangeParams,
    _: CancellationToken,
) -> Result<Option<LinkedEditingRanges>, LspError> {
    Ok(Some(LinkedEditingRanges {
        ranges: vec![
            Range::new(Position::new(9, 0), Position::new(9, 4)),
            Range::new(Position::new(10, 0), Position::new(10, 4)),
        ],
        word_pattern: Some("\\w+".to_string()),
    }))
}

async fn moniker(
    _: Arc<AppState>,
    _: ServerContext,
    _: MonikerParams,
    _: CancellationToken,
) -> Result<Option<Vec<Moniker>>, LspError> {
    Ok(Some(vec![Moniker {
        scheme: "file".to_string(),
        identifier: "main".to_string(),
        unique: UniquenessLevel::Scheme,
        kind: Some(MonikerKind::Import),
    }]))
}

fn progress(value: Option<bool>) -> lspf::types::WorkDoneProgressOptions {
    lspf::types::WorkDoneProgressOptions {
        work_done_progress: value,
    }
}

fn server() -> Server<AppState> {
    Server::builder(AppState)
        .feature(
            lspf::features::signature_help(SignatureHelpOptions {
                trigger_characters: Some(vec!["(".to_string()]),
                retrigger_characters: None,
                work_done_progress_options: Default::default(),
            }),
            signature_help,
        )
        .feature(
            lspf::features::declaration(DeclarationOptions {
                work_done_progress_options: progress(Some(true)),
            }),
            declaration,
        )
        .feature(
            lspf::features::definition(DefinitionOptions {
                work_done_progress_options: progress(Some(true)),
            }),
            definition,
        )
        .feature(
            lspf::features::type_definition(TypeDefinitionRegistrationOptions {
                static_registration_options: StaticRegistrationOptions {
                    id: Some("nav".to_string()),
                },
                ..TypeDefinitionRegistrationOptions::default()
            }),
            type_definition,
        )
        .feature(
            lspf::features::implementation(ImplementationRegistrationOptions {
                static_registration_options: StaticRegistrationOptions {
                    id: Some("nav".to_string()),
                },
                ..ImplementationRegistrationOptions::default()
            }),
            implementation,
        )
        .feature(
            lspf::features::references(ReferencesOptions {
                work_done_progress_options: progress(Some(true)),
            }),
            references,
        )
        .feature(
            lspf::features::document_highlight(DocumentHighlightOptions {
                work_done_progress_options: progress(Some(true)),
            }),
            document_highlight,
        )
        .feature(
            lspf::features::document_symbol(DocumentSymbolOptions {
                label: Some("outline".to_string()),
                work_done_progress_options: progress(Some(true)),
            }),
            document_symbol,
        )
        .feature(
            lspf::features::linked_editing_range(LinkedEditingRangeOptions {
                work_done_progress_options: progress(Some(true)),
            }),
            linked_editing_range,
        )
        .feature(
            lspf::features::moniker(MonikerOptions {
                work_done_progress_options: progress(Some(true)),
            }),
            moniker,
        )
        .build()
        .expect("navigation and lookup features build")
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

async fn drive(messages: Vec<RawMessage>) -> Vec<RawMessage> {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let transport = ChannelTransport { in_rx, out_tx };
    let mut handle = tokio::spawn(async move { server().serve(transport).await });
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

fn text_document_position(line: u32) -> Value {
    json!({
        "textDocument": { "uri": "file:///a.rs" },
        "position": { "line": line, "character": 0 }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn navigation_and_lookup_handlers_dispatch_through_the_engine() {
    let outbox = drive(vec![
        initialize_request(),
        request(2, "textDocument/signatureHelp", text_document_position(0)),
        request(3, "textDocument/declaration", text_document_position(1)),
        request(4, "textDocument/definition", text_document_position(2)),
        request(5, "textDocument/typeDefinition", text_document_position(3)),
        request(6, "textDocument/implementation", text_document_position(4)),
        request(
            7,
            "textDocument/references",
            json!({
                "textDocument": { "uri": "file:///a.rs" },
                "position": { "line": 5, "character": 0 },
                "context": { "includeDeclaration": true }
            }),
        ),
        request(
            8,
            "textDocument/documentHighlight",
            text_document_position(6),
        ),
        request(
            9,
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": "file:///a.rs" } }),
        ),
        request(
            10,
            "textDocument/linkedEditingRange",
            text_document_position(7),
        ),
        request(11, "textDocument/moniker", text_document_position(8)),
        exit(),
    ])
    .await;

    let capabilities = &result(&outbox, 1)["capabilities"];
    assert_eq!(
        capabilities["signatureHelpProvider"]["triggerCharacters"][0],
        "("
    );
    assert_eq!(
        capabilities["declarationProvider"]["workDoneProgress"],
        true
    );
    assert_eq!(capabilities["definitionProvider"]["workDoneProgress"], true);
    assert_eq!(capabilities["typeDefinitionProvider"]["id"], "nav");
    assert_eq!(capabilities["implementationProvider"]["id"], "nav");
    assert_eq!(capabilities["referencesProvider"]["workDoneProgress"], true);
    assert_eq!(
        capabilities["documentHighlightProvider"]["workDoneProgress"],
        true
    );
    assert_eq!(capabilities["documentSymbolProvider"]["label"], "outline");
    assert_eq!(
        capabilities["linkedEditingRangeProvider"]["workDoneProgress"],
        true
    );
    assert_eq!(capabilities["monikerProvider"]["workDoneProgress"], true);

    assert_eq!(result(&outbox, 2)["signatures"][0]["label"], "fn main()");
    assert_eq!(result(&outbox, 3)["uri"], "file:///decl.rs");
    assert_eq!(result(&outbox, 4)["uri"], "file:///def.rs");
    assert_eq!(result(&outbox, 5)["uri"], "file:///type.rs");
    assert_eq!(result(&outbox, 6)["uri"], "file:///impl.rs");
    assert_eq!(result(&outbox, 7)[0]["uri"], "file:///use.rs");
    assert_eq!(result(&outbox, 8)[0]["kind"], 2);
    assert_eq!(result(&outbox, 9)[0]["name"], "main");
    assert_eq!(result(&outbox, 10)["ranges"][0]["start"]["line"], 9);
    assert_eq!(result(&outbox, 11)[0]["identifier"], "main");
}

#[test]
fn duplicate_navigation_route_fails_deterministically() {
    let err = Server::builder(AppState)
        .feature(
            lspf::features::definition(DefinitionOptions {
                work_done_progress_options: progress(Some(true)),
            }),
            definition,
        )
        .feature(
            lspf::features::definition(DefinitionOptions {
                work_done_progress_options: progress(Some(true)),
            }),
            definition,
        )
        .build()
        .err()
        .expect("a repeated definition route must fail");
    assert_eq!(
        err,
        BuildError::DuplicateMethod("textDocument/definition".to_string())
    );
}
