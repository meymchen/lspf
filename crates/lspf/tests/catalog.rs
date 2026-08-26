//! Deterministic full-catalog coverage (issue #81).
//!
//! One server registers every stable LSP 3.17 feature and notification the
//! 0.3 PRD lists — the complete [`lspf::features`] catalog, the built-in
//! protocol notification hooks, and typed commands — and proves the catalog
//! boundary: the initialize response's capability JSON is byte-stable against
//! `fixtures/full_catalog_capabilities.json`, custom requests and
//! notifications contribute nothing to it, and no notebook or proposed method
//! leaks into it. Compiling this file is itself the compile-time registration
//! coverage: every listed route is registered through a typed descriptor or
//! hook, and any descriptor that loses its sealed capability contribution
//! fails the fixture comparison.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use lspf::types::notification::{
    DidChangeConfiguration, DidChangeTextDocument, DidChangeWatchedFiles,
    DidChangeWorkspaceFolders, DidCloseTextDocument, DidCreateFiles, DidDeleteFiles,
    DidOpenTextDocument, DidRenameFiles, DidSaveTextDocument, Notification, SetTrace,
    WillSaveTextDocument,
};
use lspf::types::request::{
    CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
    CodeActionRequest, CodeLensRequest, ColorPresentationRequest, Completion, DocumentColor,
    DocumentDiagnosticRequest, DocumentHighlightRequest, DocumentLinkRequest,
    DocumentSymbolRequest, FoldingRangeRequest, Formatting, GotoDeclaration, GotoDefinition,
    GotoImplementation, GotoTypeDefinition, HoverRequest, InlayHintRequest, InlineValueRequest,
    LinkedEditingRange, MonikerRequest, OnTypeFormatting, PrepareRenameRequest, RangeFormatting,
    References, Rename, Request, SelectionRangeRequest, SemanticTokensFullDeltaRequest,
    SemanticTokensFullRequest, SemanticTokensRangeRequest, SignatureHelpRequest,
    TypeHierarchyPrepare, TypeHierarchySubtypes, TypeHierarchySupertypes, WillCreateFiles,
    WillDeleteFiles, WillRenameFiles, WillSaveWaitUntil, WorkspaceDiagnosticRequest,
    WorkspaceSymbolRequest,
};
use lspf::types::{
    CallHierarchyOptions, CodeAction, CodeActionOptions, CodeLens, CodeLensOptions,
    ColorProviderOptions, CompletionItem, CompletionOptions, DeclarationOptions, DefinitionOptions,
    DiagnosticOptions, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    DocumentFormattingOptions, DocumentHighlightOptions, DocumentLink, DocumentLinkOptions,
    DocumentOnTypeFormattingOptions, DocumentRangeFormattingOptions, DocumentSymbolOptions,
    FileOperationFilter, FileOperationPattern, FileOperationPatternKind,
    FileOperationRegistrationOptions, FoldingProviderOptions, InlayHint, InlayHintOptions,
    InlineValueOptions, LinkedEditingRangeOptions, MonikerOptions, ReferencesOptions,
    RelatedFullDocumentDiagnosticReport, RenameOptions, SelectionRangeOptions, SemanticTokenType,
    SemanticTokensLegend, SemanticTokensOptions, SignatureHelpOptions,
    StaticTextDocumentRegistrationOptions, TypeHierarchyOptions, WorkDoneProgressOptions,
    WorkspaceDiagnosticReport, WorkspaceDiagnosticReportResult, WorkspaceSymbol,
    WorkspaceSymbolOptions,
};
use lspf::{
    CancellationToken, LspError, Outcome, RawMessage, RequestId, Server, ServerContext, Transport,
    TransportError, TransportReader, TransportWriter,
};

struct AppState;

// --- Typed handlers ----------------------------------------------------------

/// Generate one stub handler per marker: it decodes the typed parameters and
/// returns the fixed value, which the compile-time registration coverage needs
/// while the tests never dispatch these routes.
macro_rules! request_handlers {
    ($($name:ident: $marker:ty => $value:expr),* $(,)?) => {
        $(
            async fn $name(
                _state: Arc<AppState>,
                _ctx: ServerContext,
                _params: <$marker as Request>::Params,
                _ct: CancellationToken,
            ) -> Result<<$marker as Request>::Result, LspError> {
                Ok($value)
            }
        )*
    };
}

request_handlers! {
    hover: HoverRequest => None,
    signature_help: SignatureHelpRequest => None,
    declaration: GotoDeclaration => None,
    definition: GotoDefinition => None,
    type_definition: GotoTypeDefinition => None,
    implementation: GotoImplementation => None,
    references: References => None,
    document_highlight: DocumentHighlightRequest => None,
    document_symbol: DocumentSymbolRequest => None,
    formatting: Formatting => None,
    range_formatting: RangeFormatting => None,
    on_type_formatting: OnTypeFormatting => None,
    rename: Rename => None,
    prepare_rename: PrepareRenameRequest => None,
    document_color: DocumentColor => vec![],
    color_presentation: ColorPresentationRequest => vec![],
    folding_range: FoldingRangeRequest => None,
    selection_range: SelectionRangeRequest => None,
    linked_editing_range: LinkedEditingRange => None,
    moniker: MonikerRequest => None,
    call_hierarchy_prepare: CallHierarchyPrepare => None,
    call_hierarchy_incoming_calls: CallHierarchyIncomingCalls => None,
    call_hierarchy_outgoing_calls: CallHierarchyOutgoingCalls => None,
    type_hierarchy_prepare: TypeHierarchyPrepare => None,
    type_hierarchy_supertypes: TypeHierarchySupertypes => None,
    type_hierarchy_subtypes: TypeHierarchySubtypes => None,
    semantic_tokens_full: SemanticTokensFullRequest => None,
    semantic_tokens_full_delta: SemanticTokensFullDeltaRequest => None,
    semantic_tokens_range: SemanticTokensRangeRequest => None,
    completion: Completion => None,
    inlay_hint: InlayHintRequest => None,
    inline_value: InlineValueRequest => None,
    workspace_symbol: WorkspaceSymbolRequest => None,
    will_save_wait_until: WillSaveWaitUntil => None,
    will_create_files: WillCreateFiles => None,
    will_rename_files: WillRenameFiles => None,
    will_delete_files: WillDeleteFiles => None,
    code_action: CodeActionRequest => None,
    code_lens: CodeLensRequest => None,
    document_link: DocumentLinkRequest => None,
}

/// The resolve routes echo their typed parameters: an echo proves the route
/// decodes and encodes its marker's types without inventing fixture values
/// for result structs `lsp-types` does not give a `Default` impl.
async fn resolve_completion(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    item: CompletionItem,
    _ct: CancellationToken,
) -> Result<CompletionItem, LspError> {
    Ok(item)
}

async fn resolve_code_action(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    action: CodeAction,
    _ct: CancellationToken,
) -> Result<CodeAction, LspError> {
    Ok(action)
}

async fn resolve_code_lens(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    lens: CodeLens,
    _ct: CancellationToken,
) -> Result<CodeLens, LspError> {
    Ok(lens)
}

async fn resolve_document_link(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    link: DocumentLink,
    _ct: CancellationToken,
) -> Result<DocumentLink, LspError> {
    Ok(link)
}

async fn resolve_inlay_hint(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    hint: InlayHint,
    _ct: CancellationToken,
) -> Result<InlayHint, LspError> {
    Ok(hint)
}

async fn resolve_workspace_symbol(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    symbol: WorkspaceSymbol,
    _ct: CancellationToken,
) -> Result<WorkspaceSymbol, LspError> {
    Ok(symbol)
}

async fn document_diagnostic(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    _params: <DocumentDiagnosticRequest as Request>::Params,
    _ct: CancellationToken,
) -> Result<DocumentDiagnosticReportResult, LspError> {
    Ok(DocumentDiagnosticReportResult::Report(
        DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport::default()),
    ))
}

async fn workspace_diagnostic(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    _params: <WorkspaceDiagnosticRequest as Request>::Params,
    _ct: CancellationToken,
) -> Result<WorkspaceDiagnosticReportResult, LspError> {
    Ok(WorkspaceDiagnosticReportResult::Report(
        WorkspaceDiagnosticReport::default(),
    ))
}

/// Generate one stub notification handler per marker.
macro_rules! notification_handlers {
    ($($name:ident: $marker:ty),* $(,)?) => {
        $(
            async fn $name(
                _state: Arc<AppState>,
                _ctx: ServerContext,
                _params: <$marker as Notification>::Params,
            ) {
            }
        )*
    };
}

notification_handlers! {
    did_open: DidOpenTextDocument,
    did_change: DidChangeTextDocument,
    did_close: DidCloseTextDocument,
    will_save: WillSaveTextDocument,
    did_save: DidSaveTextDocument,
    did_change_configuration: DidChangeConfiguration,
    did_change_workspace_folders: DidChangeWorkspaceFolders,
    set_trace: SetTrace,
    did_change_watched_files: DidChangeWatchedFiles,
    did_create_files: DidCreateFiles,
    did_rename_files: DidRenameFiles,
    did_delete_files: DidDeleteFiles,
}

async fn noop_command(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    _args: Vec<String>,
    _ct: CancellationToken,
) -> Result<(), LspError> {
    Ok(())
}

// --- A custom route outside the stable catalog --------------------------------

enum CatalogExtension {}

impl Request for CatalogExtension {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "catalog/extension";
}

enum CatalogNotice {}

impl Notification for CatalogNotice {
    type Params = Value;
    const METHOD: &'static str = "catalog/notice";
}

async fn custom_request(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    _params: Value,
    _ct: CancellationToken,
) -> Result<Value, LspError> {
    Ok(json!(null))
}

async fn custom_notification(_state: Arc<AppState>, _ctx: ServerContext, _params: Value) {}

// --- Fixed options for the whole catalog --------------------------------------

fn work_done_options() -> WorkDoneProgressOptions {
    WorkDoneProgressOptions::default()
}

fn semantic_options() -> SemanticTokensOptions {
    SemanticTokensOptions {
        work_done_progress_options: work_done_options(),
        legend: SemanticTokensLegend {
            token_types: vec![SemanticTokenType::KEYWORD],
            token_modifiers: vec![],
        },
        range: None,
        full: None,
    }
}

fn file_filters() -> FileOperationRegistrationOptions {
    FileOperationRegistrationOptions {
        filters: vec![FileOperationFilter {
            scheme: Some("file".to_string()),
            pattern: FileOperationPattern {
                glob: "**/*.rs".to_string(),
                matches: Some(FileOperationPatternKind::File),
                options: None,
            },
        }],
    }
}

/// Apply the fixed full-catalog registrations to `builder` and hand the
/// builder back, so tests can append further registrations before `build`.
fn catalog_registrations(builder: lspf::ServerBuilder<AppState>) -> lspf::ServerBuilder<AppState> {
    builder
        // Text document requests.
        .feature(lspf::features::hover(), hover)
        .feature(
            lspf::features::signature_help(SignatureHelpOptions {
                trigger_characters: Some(vec!["(".to_string()]),
                ..SignatureHelpOptions::default()
            }),
            signature_help,
        )
        .feature(
            lspf::features::declaration(DeclarationOptions {
                work_done_progress_options: work_done_options(),
            }),
            declaration,
        )
        .feature(
            lspf::features::definition(DefinitionOptions {
                work_done_progress_options: work_done_options(),
            }),
            definition,
        )
        .feature(
            lspf::features::type_definition(StaticTextDocumentRegistrationOptions {
                document_selector: None,
                id: None,
            }),
            type_definition,
        )
        .feature(
            lspf::features::implementation(StaticTextDocumentRegistrationOptions {
                document_selector: None,
                id: None,
            }),
            implementation,
        )
        .feature(
            lspf::features::references(ReferencesOptions {
                work_done_progress_options: work_done_options(),
            }),
            references,
        )
        .feature(
            lspf::features::document_highlight(DocumentHighlightOptions {
                work_done_progress_options: work_done_options(),
            }),
            document_highlight,
        )
        .feature(
            lspf::features::document_symbol(DocumentSymbolOptions {
                work_done_progress_options: work_done_options(),
                label: None,
            }),
            document_symbol,
        )
        .feature(
            lspf::features::code_action(CodeActionOptions::default()),
            code_action,
        )
        .feature(lspf::features::code_action_resolve(), resolve_code_action)
        .feature(
            lspf::features::code_lens(CodeLensOptions { resolve_provider: None }),
            code_lens,
        )
        .feature(lspf::features::code_lens_resolve(), resolve_code_lens)
        .feature(
            lspf::features::document_link(DocumentLinkOptions {
                work_done_progress_options: work_done_options(),
                resolve_provider: None,
            }),
            document_link,
        )
        .feature(
            lspf::features::document_link_resolve(),
            resolve_document_link,
        )
        .feature(
            lspf::features::document_formatting(DocumentFormattingOptions::default()),
            formatting,
        )
        .feature(
            lspf::features::range_formatting(DocumentRangeFormattingOptions {
                work_done_progress_options: work_done_options(),
            }),
            range_formatting,
        )
        .feature(
            lspf::features::on_type_formatting(DocumentOnTypeFormattingOptions::default()),
            on_type_formatting,
        )
        .feature(
            lspf::features::rename(RenameOptions {
                work_done_progress_options: work_done_options(),
                prepare_provider: None,
            }),
            rename,
        )
        .feature(lspf::features::prepare_rename(), prepare_rename)
        .feature(
            lspf::features::document_color(ColorProviderOptions {}),
            document_color,
        )
        .feature(lspf::features::color_presentation(), color_presentation)
        .feature(
            lspf::features::folding_range(FoldingProviderOptions {}),
            folding_range,
        )
        .feature(
            lspf::features::selection_range(SelectionRangeOptions::default()),
            selection_range,
        )
        .feature(
            lspf::features::linked_editing_range(LinkedEditingRangeOptions::default()),
            linked_editing_range,
        )
        .feature(
            lspf::features::moniker(MonikerOptions {
                work_done_progress_options: work_done_options(),
            }),
            moniker,
        )
        .feature(
            lspf::features::call_hierarchy_prepare(CallHierarchyOptions::default()),
            call_hierarchy_prepare,
        )
        .feature(
            lspf::features::call_hierarchy_incoming_calls(),
            call_hierarchy_incoming_calls,
        )
        .feature(
            lspf::features::call_hierarchy_outgoing_calls(),
            call_hierarchy_outgoing_calls,
        )
        .feature(
            lspf::features::type_hierarchy_prepare(TypeHierarchyOptions::default()),
            type_hierarchy_prepare,
        )
        .feature(
            lspf::features::type_hierarchy_supertypes(),
            type_hierarchy_supertypes,
        )
        .feature(
            lspf::features::type_hierarchy_subtypes(),
            type_hierarchy_subtypes,
        )
        .feature(
            lspf::features::semantic_tokens_full(semantic_options()),
            semantic_tokens_full,
        )
        .feature(
            lspf::features::semantic_tokens_full_delta(semantic_options()),
            semantic_tokens_full_delta,
        )
        .feature(
            lspf::features::semantic_tokens_range(semantic_options()),
            semantic_tokens_range,
        )
        .feature(
            lspf::features::completion(CompletionOptions {
                trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
                ..CompletionOptions::default()
            }),
            completion,
        )
        .feature(lspf::features::completion_resolve(), resolve_completion)
        .feature(
            lspf::features::inlay_hint(InlayHintOptions::default()),
            inlay_hint,
        )
        .feature(
            lspf::features::inlay_hint_resolve(),
            resolve_inlay_hint,
        )
        .feature(
            lspf::features::inline_value(InlineValueOptions::default()),
            inline_value,
        )
        .feature(
            lspf::features::document_diagnostic(DiagnosticOptions {
                identifier: Some("catalog".to_string()),
                workspace_diagnostics: true,
                ..DiagnosticOptions::default()
            }),
            document_diagnostic,
        )
        .feature(
            lspf::features::workspace_diagnostic(DiagnosticOptions {
                identifier: Some("catalog".to_string()),
                workspace_diagnostics: true,
                ..DiagnosticOptions::default()
            }),
            workspace_diagnostic,
        )
        .feature(lspf::features::will_save_wait_until(), will_save_wait_until)
        // Workspace requests.
        .feature(
            lspf::features::workspace_symbol(WorkspaceSymbolOptions {
                work_done_progress_options: work_done_options(),
                resolve_provider: None,
            }),
            workspace_symbol,
        )
        .feature(
            lspf::features::workspace_symbol_resolve(),
            resolve_workspace_symbol,
        )
        .feature(
            lspf::features::will_create_files(file_filters()),
            will_create_files,
        )
        .feature(
            lspf::features::will_rename_files(file_filters()),
            will_rename_files,
        )
        .feature(
            lspf::features::will_delete_files(file_filters()),
            will_delete_files,
        )
        // Notification features (file operations and watched files).
        .feature_notification(
            lspf::features::did_create_files(file_filters()),
            did_create_files,
        )
        .feature_notification(
            lspf::features::did_rename_files(file_filters()),
            did_rename_files,
        )
        .feature_notification(
            lspf::features::did_delete_files(file_filters()),
            did_delete_files,
        )
        .feature_notification(
            lspf::features::did_change_watched_files(),
            did_change_watched_files,
        )
        // Built-in protocol notifications: typed post-validation hooks.
        .notification::<DidOpenTextDocument, _, _>(did_open)
        .notification::<DidChangeTextDocument, _, _>(did_change)
        .notification::<DidCloseTextDocument, _, _>(did_close)
        .notification::<WillSaveTextDocument, _, _>(will_save)
        .notification::<DidSaveTextDocument, _, _>(did_save)
        .notification::<DidChangeConfiguration, _, _>(did_change_configuration)
        .notification::<DidChangeWorkspaceFolders, _, _>(did_change_workspace_folders)
        .notification::<SetTrace, _, _>(set_trace)
        // Commands, in a fixed registration order.
        .command::<Vec<String>, (), _, _>("catalog.two", noop_command)
        .command::<Vec<String>, (), _, _>("catalog.one", noop_command)
}

/// Register every stable 3.17 feature, notification hook, and command the
/// 0.3 PRD lists, with fixed options, and return the built server.
fn full_catalog() -> Server<AppState> {
    catalog_registrations(Server::builder(AppState))
        .build()
        .expect("the full stable catalog registers without conflict")
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

fn initialize_request(id: i32) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed("initialize"),
        params: Bytes::from(
            serde_json::to_vec(&json!({
                "processId": null, "rootUri": null, "capabilities": {}
            }))
            .unwrap(),
        ),
    }
}

fn exit() -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed("exit"),
        params: Bytes::from_static(b"null"),
    }
}

/// Drive `server` through `initialize` and `exit`, and return the raw wire
/// body of the initialize response plus the session outcome.
async fn initialize_wire<S: Send + Sync + 'static>(server: Server<S>) -> (String, Outcome) {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let transport = ChannelTransport { in_rx, out_tx };
    let mut handle = tokio::spawn(async move { server.serve(transport).await });

    in_tx.send(initialize_request(1)).unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), out_rx.recv())
        .await
        .expect("initialize response within 2s")
        .expect("outgoing channel open");
    let RawMessage::Response {
        result: Ok(bytes), ..
    } = response
    else {
        panic!("expected a successful initialize response");
    };
    in_tx.send(exit()).unwrap();

    let outcome = tokio::time::timeout(Duration::from_secs(2), &mut handle)
        .await
        .expect("serve returned within 2s")
        .expect("server task did not panic")
        .expect("serve ended cleanly");

    (String::from_utf8(bytes.to_vec()).unwrap(), outcome)
}

// --- Tests -------------------------------------------------------------------

/// Every stable feature registered together produces one deterministic
/// capability object, verified byte-for-byte against the fixture. Any added,
/// renamed, or reordered capability field — or a descriptor whose contribution
/// drifted — breaks here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_full_catalog_advertises_a_byte_stable_capability_snapshot() {
    let (wire, outcome) = initialize_wire(full_catalog()).await;
    assert_eq!(outcome, Outcome::Exit { code: 1 });

    let fixture = include_str!("fixtures/full_catalog_capabilities.json").trim_end();
    assert_eq!(
        wire, fixture,
        "the full-catalog capability snapshot must stay byte-stable; \
         update the fixture only with a deliberate capability change"
    );
}

/// Custom requests and notifications are still registrable beside the whole
/// stable catalog and contribute nothing to it: the capability bytes are
/// identical with or without them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_registrations_contribute_nothing_to_the_full_catalog() {
    let (without, _) = initialize_wire(full_catalog()).await;
    let (with, _) = initialize_wire(
        catalog_registrations(Server::builder(AppState))
            .request::<CatalogExtension, _, _>(custom_request)
            .notification::<CatalogNotice, _, _>(custom_notification)
            .build()
            .expect("custom routes build beside the catalog"),
    )
    .await;
    assert_eq!(
        with, without,
        "a custom request and notification change no capability byte"
    );
}

/// The default catalog stays on stable LSP 3.17: no notebook method, and no
/// proposed or 3.18-draft field, appears anywhere in the capability JSON.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_notebook_or_proposed_fields_leak_into_the_catalog() {
    let (wire, _) = initialize_wire(full_catalog()).await;
    assert!(
        !wire.to_lowercase().contains("notebook"),
        "the stable catalog advertises no notebook capability: {wire}"
    );
    // Proposed/3.18-draft method families the 0.3 PRD excludes.
    for field in [
        "textDocumentContent",
        "pullOnTypeFormatting",
        "codeActionResolveOptions",
    ] {
        assert!(
            !wire.contains(field),
            "proposed or 3.18-draft field {field:?} must not appear in the catalog: {wire}"
        );
    }
}

/// The default catalog — no registrations at all — advertises only the
/// protocol-owned fields: the negotiated position encoding, document sync,
/// and workspace-folder support. No notebook or proposed capability appears.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_default_catalog_advertises_no_notebook_or_proposed_capability() {
    let (wire, _) = initialize_wire(
        Server::builder(AppState)
            .build()
            .expect("an empty server builds"),
    )
    .await;
    assert!(
        !wire.to_lowercase().contains("notebook"),
        "the default catalog advertises no notebook capability: {wire}"
    );
    for field in ["textDocumentContent", "pullOnTypeFormatting"] {
        assert!(
            !wire.contains(field),
            "proposed or 3.18-draft field {field:?} must not appear by default: {wire}"
        );
    }
}

/// Command names merge into one execute-command capability preserving
/// registration order even in the middle of the full catalog.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_full_catalog_keeps_command_registration_order() {
    let (wire, _) = initialize_wire(full_catalog()).await;
    let parsed: Value = serde_json::from_str(&wire).unwrap();
    assert_eq!(
        parsed["capabilities"]["executeCommandProvider"]["commands"],
        json!(["catalog.two", "catalog.one"]),
        "the command list is de-duplicated and registration-order stable"
    );
}

/// Enabling the `proposed` Cargo feature must not move the stable catalog
/// boundary (issue #108): the full-catalog capability bytes stay pinned to the
/// fixture and no notebook capability appears. The proposed refresh helpers
/// are outgoing-only `ClientHandle` calls, so the Router catalog cannot change.
#[cfg(feature = "proposed")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_proposed_feature_leaves_the_stable_catalog_untouched() {
    let (wire, outcome) = initialize_wire(full_catalog()).await;
    assert_eq!(outcome, Outcome::Exit { code: 1 });

    let fixture = include_str!("fixtures/full_catalog_capabilities.json").trim_end();
    assert_eq!(
        wire, fixture,
        "enabling `proposed` must not change the stable capability snapshot"
    );
    assert!(
        !wire.to_lowercase().contains("notebook"),
        "enabling `proposed` must not advertise notebook support: {wire}"
    );
}
