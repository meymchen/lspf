//! End-to-end coverage for the resolve- and prepare-capable text-document
//! capability families (issue #74).
//!
//! Each family registers a base feature and its resolve or prepare companion:
//! both routes dispatch typed values while one family-aware merge emits a
//! single deterministic capability — verified byte-for-byte against the
//! fixtures for rename plus prepare and the code-action resolve family.
//! Static and initialize-conditional registrations share the same merge and
//! validation rules, so a dependent registered without its base fails the
//! initialize transaction exactly as a static one fails `build`.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use tokio::sync::mpsc;

use lspf::types::{
    CodeAction, CodeActionKind, CodeActionOptions, CodeActionParams, CodeActionProviderCapability,
    CodeActionResponse, CodeLens, CodeLensOptions, CodeLensParams, DocumentLink,
    DocumentLinkOptions, DocumentLinkParams, InlayHint, InlayHintKind, InlayHintLabel,
    InlayHintOptions, InlayHintParams, InlayHintServerCapabilities, OneOf, PrepareRenameResponse,
    RenameOptions, RenameParams, TextDocumentPositionParams, WorkspaceEdit,
};
use lspf::{
    BuildError, CancellationToken, Context, LspError, RawMessage, RequestId, Server, Transport,
    TransportError, TransportReader, TransportWriter,
};

/// Application state shared as `Arc<S>` by every handler on the connection.
struct AppState;

async fn rename(
    _state: Arc<AppState>,
    _ctx: Context,
    _params: RenameParams,
    _ct: CancellationToken,
) -> Result<Option<WorkspaceEdit>, LspError> {
    Ok(Some(WorkspaceEdit::default()))
}

async fn prepare_rename(
    _state: Arc<AppState>,
    _ctx: Context,
    params: TextDocumentPositionParams,
    _ct: CancellationToken,
) -> Result<Option<PrepareRenameResponse>, LspError> {
    Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
        range: lspf::types::Range::new(params.position, params.position),
        placeholder: "ident".to_string(),
    }))
}

fn rename_options() -> RenameOptions {
    RenameOptions {
        prepare_provider: None,
        work_done_progress_options: lspf::types::WorkDoneProgressOptions {
            work_done_progress: Some(true),
        },
    }
}

async fn code_action(
    _state: Arc<AppState>,
    _ctx: Context,
    _params: CodeActionParams,
    _ct: CancellationToken,
) -> Result<Option<CodeActionResponse>, LspError> {
    Ok(Some(vec![lspf::types::CodeActionOrCommand::CodeAction(
        CodeAction {
            title: "Fix it".to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            ..CodeAction::default()
        },
    )]))
}

async fn code_action_resolve(
    _state: Arc<AppState>,
    _ctx: Context,
    mut action: CodeAction,
    _ct: CancellationToken,
) -> Result<CodeAction, LspError> {
    action.is_preferred = Some(true);
    Ok(action)
}

fn code_action_options() -> CodeActionOptions {
    CodeActionOptions {
        code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
        ..CodeActionOptions::default()
    }
}

async fn code_lens(
    _state: Arc<AppState>,
    _ctx: Context,
    _params: CodeLensParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<CodeLens>>, LspError> {
    Ok(Some(vec![CodeLens {
        range: lspf::types::Range::new(
            lspf::types::Position::new(0, 0),
            lspf::types::Position::new(0, 4),
        ),
        command: None,
        data: None,
    }]))
}

async fn code_lens_resolve(
    _state: Arc<AppState>,
    _ctx: Context,
    mut lens: CodeLens,
    _ct: CancellationToken,
) -> Result<CodeLens, LspError> {
    lens.command = Some(lspf::types::Command::new(
        "show refs".to_string(),
        "editor.showRefs".to_string(),
        None,
    ));
    Ok(lens)
}

fn code_lens_options() -> CodeLensOptions {
    CodeLensOptions {
        resolve_provider: None,
    }
}

async fn document_link(
    _state: Arc<AppState>,
    _ctx: Context,
    _params: DocumentLinkParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<DocumentLink>>, LspError> {
    Ok(Some(vec![DocumentLink {
        range: lspf::types::Range::new(
            lspf::types::Position::new(0, 0),
            lspf::types::Position::new(0, 4),
        ),
        target: None,
        tooltip: Some("docs".to_string()),
        data: None,
    }]))
}

async fn document_link_resolve(
    _state: Arc<AppState>,
    _ctx: Context,
    mut link: DocumentLink,
    _ct: CancellationToken,
) -> Result<DocumentLink, LspError> {
    link.target = Some("https://example.com/docs".parse().expect("a valid URI"));
    Ok(link)
}

fn document_link_options() -> DocumentLinkOptions {
    DocumentLinkOptions {
        resolve_provider: None,
        work_done_progress_options: Default::default(),
    }
}

async fn inlay_hint(
    _state: Arc<AppState>,
    _ctx: Context,
    _params: InlayHintParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<InlayHint>>, LspError> {
    Ok(Some(vec![InlayHint {
        position: lspf::types::Position::new(0, 4),
        label: InlayHintLabel::String(": u32".to_string()),
        kind: Some(InlayHintKind::TYPE),
        text_edits: None,
        tooltip: None,
        padding_left: None,
        padding_right: None,
        data: None,
    }]))
}

async fn inlay_hint_resolve(
    _state: Arc<AppState>,
    _ctx: Context,
    mut hint: InlayHint,
    _ct: CancellationToken,
) -> Result<InlayHint, LspError> {
    hint.tooltip = Some(lspf::types::InlayHintTooltip::String(
        "the inferred type".to_string(),
    ));
    Ok(hint)
}

fn inlay_hint_options() -> InlayHintOptions {
    InlayHintOptions::default()
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

fn initialize_result(outbox: &[RawMessage], id: i32) -> lspf::types::InitializeResult {
    serde_json::from_value(ok_result(outbox, id).expect("initialize response"))
        .expect("the initialize result decodes")
}

fn rename_provider(outbox: &[RawMessage], id: i32) -> RenameOptions {
    match initialize_result(outbox, id).capabilities.rename_provider {
        Some(OneOf::Right(options)) => options,
        other => panic!("expected one renameProvider options object, got {other:?}"),
    }
}

fn code_action_provider(outbox: &[RawMessage], id: i32) -> CodeActionOptions {
    match initialize_result(outbox, id)
        .capabilities
        .code_action_provider
    {
        Some(CodeActionProviderCapability::Options(options)) => options,
        other => panic!("expected one codeActionProvider options object, got {other:?}"),
    }
}

fn code_lens_provider(outbox: &[RawMessage], id: i32) -> CodeLensOptions {
    initialize_result(outbox, id)
        .capabilities
        .code_lens_provider
        .expect("the family advertises one codeLensProvider capability")
}

fn document_link_provider(outbox: &[RawMessage], id: i32) -> DocumentLinkOptions {
    initialize_result(outbox, id)
        .capabilities
        .document_link_provider
        .expect("the family advertises one documentLinkProvider capability")
}

fn inlay_hint_provider(outbox: &[RawMessage], id: i32) -> InlayHintOptions {
    match initialize_result(outbox, id)
        .capabilities
        .inlay_hint_provider
    {
        Some(OneOf::Right(InlayHintServerCapabilities::Options(options))) => options,
        other => panic!("expected one inlayHintProvider options object, got {other:?}"),
    }
}

/// The raw wire bytes of a successful response, for byte-stable fixture
/// comparisons against what the client actually receives.
fn wire_result(outbox: &[RawMessage], id: i32) -> String {
    match response(outbox, id).expect("response") {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => String::from_utf8(bytes.to_vec()).unwrap(),
        other => panic!("expected a successful response, got {other:?}"),
    }
}

// --- Rename and prepare rename -----------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_and_prepare_dispatch_typed_values_and_merge_one_capability() {
    let server = Server::builder(AppState)
        .feature(lspf::features::rename(rename_options()), rename)
        .feature(lspf::features::prepare_rename(), prepare_rename)
        .build()
        .expect("rename and prepare rename build");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            request(
                2,
                "textDocument/rename",
                json!({
                    "textDocument": { "uri": "file:///a.rs" },
                    "position": { "line": 0, "character": 4 },
                    "newName": "renamed"
                }),
            ),
            request(
                3,
                "textDocument/prepareRename",
                json!({
                    "textDocument": { "uri": "file:///a.rs" },
                    "position": { "line": 0, "character": 4 }
                }),
            ),
            exit(),
        ],
    )
    .await;

    // The family merge emits one capability carrying both contributions.
    let merged = rename_provider(&outbox, 1);
    assert_eq!(merged.prepare_provider, Some(true));
    assert_eq!(
        merged.work_done_progress_options.work_done_progress,
        Some(true),
        "the base feature's options survive the family merge"
    );

    // Both routes dispatch typed values.
    let edit: WorkspaceEdit =
        serde_json::from_value(ok_result(&outbox, 2).expect("rename response")).unwrap();
    assert_eq!(edit, WorkspaceEdit::default());
    let prepared: PrepareRenameResponse =
        serde_json::from_value(ok_result(&outbox, 3).expect("prepare response")).unwrap();
    match prepared {
        PrepareRenameResponse::RangeWithPlaceholder { placeholder, .. } => {
            assert_eq!(placeholder, "ident")
        }
        other => panic!("expected a range with placeholder, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_prepare_uses_the_same_merge() {
    let server = Server::builder(AppState)
        .feature(lspf::features::rename(rename_options()), rename)
        .configure_initialize(|_params, registrar| {
            registrar.feature(lspf::features::prepare_rename(), prepare_rename);
            Ok(())
        })
        .build()
        .expect("a static base with a conditional prepare builds");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;

    let merged = rename_provider(&outbox, 1);
    assert_eq!(
        merged.prepare_provider,
        Some(true),
        "conditional registrations merge through the same family rules"
    );
    assert_eq!(
        merged.work_done_progress_options.work_done_progress,
        Some(true)
    );
}

#[test]
fn static_prepare_without_rename_fails_build() {
    let err = Server::builder(AppState)
        .feature(lspf::features::prepare_rename(), prepare_rename)
        .build()
        .err()
        .expect("a dangling prepare contribution must fail build");
    assert_eq!(
        err,
        BuildError::ConflictingCapability {
            field: "renameProvider"
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_prepare_without_rename_fails_initialization() {
    let server = Server::builder(AppState)
        .configure_initialize(|_params, registrar| {
            registrar.feature(lspf::features::prepare_rename(), prepare_rename);
            Ok(())
        })
        .build()
        .expect("build does not run the conditional transaction");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;

    assert_eq!(
        error_code(&outbox, 1),
        Some(-32603),
        "combined validation rejects the dangling prepare with InternalError"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merged_rename_provider_is_byte_stable() {
    let server = Server::builder(AppState)
        .feature(lspf::features::rename(rename_options()), rename)
        .feature(lspf::features::prepare_rename(), prepare_rename)
        .build()
        .expect("rename and prepare rename build");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;

    // Compare against the raw wire bytes, not a re-serialized typed value, so
    // an added, renamed, or reordered field in the emitted object breaks here.
    let wire = wire_result(&outbox, 1);
    let fixture = include_str!("fixtures/rename_provider_with_prepare.json").trim_end();
    assert!(
        wire.contains(&format!("\"renameProvider\":{fixture}")),
        "the merged renameProvider on the wire must stay byte-stable; \
         update the fixture only with a deliberate capability change.\nwire: {wire}"
    );
}

// --- Code action and code-action resolve --------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_action_and_resolve_dispatch_typed_values_and_merge_one_capability() {
    let server = Server::builder(AppState)
        .feature(
            lspf::features::code_action(code_action_options()),
            code_action,
        )
        .feature(lspf::features::code_action_resolve(), code_action_resolve)
        .build()
        .expect("code action and resolve build");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            request(
                2,
                "textDocument/codeAction",
                json!({
                    "textDocument": { "uri": "file:///a.rs" },
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 4 }
                    },
                    "context": { "diagnostics": [] }
                }),
            ),
            request(3, "codeAction/resolve", json!({ "title": "Fix it" })),
            exit(),
        ],
    )
    .await;

    // The family merge emits one capability carrying both contributions.
    let merged = code_action_provider(&outbox, 1);
    assert_eq!(merged.resolve_provider, Some(true));
    assert_eq!(
        merged.code_action_kinds,
        Some(vec![CodeActionKind::QUICKFIX]),
        "the base feature's options survive the family merge"
    );

    // Both routes dispatch typed values.
    let actions: CodeActionResponse =
        serde_json::from_value(ok_result(&outbox, 2).expect("code action response")).unwrap();
    match &actions[0] {
        lspf::types::CodeActionOrCommand::CodeAction(action) => {
            assert_eq!(action.title, "Fix it");
            assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
        }
        other => panic!("expected a code action, got {other:?}"),
    }
    let resolved: CodeAction =
        serde_json::from_value(ok_result(&outbox, 3).expect("resolve response")).unwrap();
    assert_eq!(resolved.title, "Fix it");
    assert_eq!(resolved.is_preferred, Some(true));
}

#[test]
fn static_code_action_resolve_without_code_action_fails_build() {
    let err = Server::builder(AppState)
        .feature(lspf::features::code_action_resolve(), code_action_resolve)
        .build()
        .err()
        .expect("a dangling resolve contribution must fail build");
    assert_eq!(
        err,
        BuildError::ConflictingCapability {
            field: "codeActionProvider"
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_code_action_resolve_without_code_action_fails_initialization() {
    let server = Server::builder(AppState)
        .configure_initialize(|_params, registrar| {
            registrar.feature(lspf::features::code_action_resolve(), code_action_resolve);
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
async fn merged_code_action_provider_is_byte_stable() {
    let server = Server::builder(AppState)
        .feature(
            lspf::features::code_action(code_action_options()),
            code_action,
        )
        .feature(lspf::features::code_action_resolve(), code_action_resolve)
        .build()
        .expect("code action and resolve build");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;

    // Compare against the raw wire bytes, not a re-serialized typed value, so
    // an added, renamed, or reordered field in the emitted object breaks here.
    let wire = wire_result(&outbox, 1);
    let fixture = include_str!("fixtures/code_action_provider_with_resolve.json").trim_end();
    assert!(
        wire.contains(&format!("\"codeActionProvider\":{fixture}")),
        "the merged codeActionProvider on the wire must stay byte-stable; \
         update the fixture only with a deliberate capability change.\nwire: {wire}"
    );
}

// --- Code lens and code-lens resolve ------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_lens_and_resolve_dispatch_typed_values_and_merge_one_capability() {
    let server = Server::builder(AppState)
        .feature(lspf::features::code_lens(code_lens_options()), code_lens)
        .feature(lspf::features::code_lens_resolve(), code_lens_resolve)
        .build()
        .expect("code lens and resolve build");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            request(
                2,
                "textDocument/codeLens",
                json!({ "textDocument": { "uri": "file:///a.rs" } }),
            ),
            request(
                3,
                "codeLens/resolve",
                json!({
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 4 }
                    }
                }),
            ),
            exit(),
        ],
    )
    .await;

    // The family merge emits one capability carrying both contributions.
    let merged = code_lens_provider(&outbox, 1);
    assert_eq!(merged.resolve_provider, Some(true));

    // Both routes dispatch typed values.
    let lenses: Vec<CodeLens> =
        serde_json::from_value(ok_result(&outbox, 2).expect("code lens response")).unwrap();
    assert_eq!(lenses.len(), 1);
    assert_eq!(lenses[0].command, None);
    let resolved: CodeLens =
        serde_json::from_value(ok_result(&outbox, 3).expect("resolve response")).unwrap();
    let command = resolved.command.expect("resolve fills in the command");
    assert_eq!(command.command, "editor.showRefs");
}

#[test]
fn static_code_lens_resolve_without_code_lens_fails_build() {
    let err = Server::builder(AppState)
        .feature(lspf::features::code_lens_resolve(), code_lens_resolve)
        .build()
        .err()
        .expect("a dangling resolve contribution must fail build");
    assert_eq!(
        err,
        BuildError::ConflictingCapability {
            field: "codeLensProvider"
        }
    );
}

// --- Document link and document-link resolve ----------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_link_and_resolve_dispatch_typed_values_and_merge_one_capability() {
    let server = Server::builder(AppState)
        .feature(
            lspf::features::document_link(document_link_options()),
            document_link,
        )
        .feature(
            lspf::features::document_link_resolve(),
            document_link_resolve,
        )
        .build()
        .expect("document link and resolve build");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            request(
                2,
                "textDocument/documentLink",
                json!({ "textDocument": { "uri": "file:///a.rs" } }),
            ),
            request(
                3,
                "documentLink/resolve",
                json!({
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 4 }
                    }
                }),
            ),
            exit(),
        ],
    )
    .await;

    // The family merge emits one capability carrying both contributions.
    let merged = document_link_provider(&outbox, 1);
    assert_eq!(merged.resolve_provider, Some(true));

    // Both routes dispatch typed values.
    let links: Vec<DocumentLink> =
        serde_json::from_value(ok_result(&outbox, 2).expect("document link response")).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target, None);
    assert_eq!(links[0].tooltip.as_deref(), Some("docs"));
    let resolved: DocumentLink =
        serde_json::from_value(ok_result(&outbox, 3).expect("resolve response")).unwrap();
    let target = resolved.target.expect("resolve fills in the target");
    assert_eq!(target.as_str(), "https://example.com/docs");
}

#[test]
fn static_document_link_resolve_without_document_link_fails_build() {
    let err = Server::builder(AppState)
        .feature(
            lspf::features::document_link_resolve(),
            document_link_resolve,
        )
        .build()
        .err()
        .expect("a dangling resolve contribution must fail build");
    assert_eq!(
        err,
        BuildError::ConflictingCapability {
            field: "documentLinkProvider"
        }
    );
}

// --- Inlay hint and inlay-hint resolve ----------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inlay_hint_and_resolve_dispatch_typed_values_and_merge_one_capability() {
    let server = Server::builder(AppState)
        .feature(lspf::features::inlay_hint(inlay_hint_options()), inlay_hint)
        .feature(lspf::features::inlay_hint_resolve(), inlay_hint_resolve)
        .build()
        .expect("inlay hint and resolve build");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            request(
                2,
                "textDocument/inlayHint",
                json!({
                    "textDocument": { "uri": "file:///a.rs" },
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 1, "character": 0 }
                    }
                }),
            ),
            request(
                3,
                "inlayHint/resolve",
                json!({
                    "position": { "line": 0, "character": 4 },
                    "label": ": u32"
                }),
            ),
            exit(),
        ],
    )
    .await;

    // The family merge emits one capability carrying both contributions.
    let merged = inlay_hint_provider(&outbox, 1);
    assert_eq!(merged.resolve_provider, Some(true));

    // Both routes dispatch typed values.
    let hints: Vec<InlayHint> =
        serde_json::from_value(ok_result(&outbox, 2).expect("inlay hint response")).unwrap();
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].kind, Some(InlayHintKind::TYPE));
    assert!(hints[0].tooltip.is_none());
    let resolved: InlayHint =
        serde_json::from_value(ok_result(&outbox, 3).expect("resolve response")).unwrap();
    match resolved.tooltip {
        Some(lspf::types::InlayHintTooltip::String(tooltip)) => {
            assert_eq!(tooltip, "the inferred type")
        }
        other => panic!("expected a string tooltip, got {other:?}"),
    }
}

#[test]
fn static_inlay_hint_resolve_without_inlay_hint_fails_build() {
    let err = Server::builder(AppState)
        .feature(lspf::features::inlay_hint_resolve(), inlay_hint_resolve)
        .build()
        .err()
        .expect("a dangling resolve contribution must fail build");
    assert_eq!(
        err,
        BuildError::ConflictingCapability {
            field: "inlayHintProvider"
        }
    );
}
