//! Deterministic full-catalog coverage (issues #81 and #242).
//!
//! One server registers every feature and notification the [`lspf::features`]
//! catalog offers — plus the built-in protocol notification hooks and typed
//! commands — and proves the catalog boundary two ways. Byte-stability: the
//! initialize response's capability JSON matches
//! `fixtures/full_catalog_capabilities.json` exactly, and custom requests and
//! notifications contribute nothing to it. Spec coverage: the capability
//! fields it advertises are measured against the vendored LSP 3.18.0
//! metaModel in `fixtures/lsp_meta_model_3_18_0.json`, which makes the
//! question "does the catalog still match the spec?" a failing test rather
//! than something a reader has to check by hand.
//!
//! Compiling this file is itself the compile-time registration coverage:
//! every listed route is registered through a typed descriptor or hook, and
//! any descriptor that loses its sealed capability contribution fails the
//! fixture comparison.
//!
//! Measuring against 3.18 contradicts ADR 0024, whose stable-catalog boundary
//! still reads "no notebook method and no proposed or 3.18-draft method". The
//! 3.18 programme overturns that on purpose: issue #249 records the ADR that
//! supersedes ADR 0024's notebook exclusion, and issue #258 restates the
//! boundary across the ADRs, README, and guides. Until those land, ADR 0024's
//! *default*-catalog invariant is the one still enforced here, by
//! [`the_default_catalog_advertises_only_protocol_owned_fields`].

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use lspf::types::{
    CallHierarchyIncomingCallsRequest as CallHierarchyIncomingCalls, CallHierarchyOptions,
    CallHierarchyOutgoingCallsRequest as CallHierarchyOutgoingCalls,
    CallHierarchyPrepareRequest as CallHierarchyPrepare, CodeAction, CodeActionOptions,
    CodeActionRequest, CodeLens, CodeLensOptions, CodeLensRequest, ColorPresentationRequest,
    CompletionItem, CompletionOptions, CompletionRequest as Completion, DeclarationOptions,
    DeclarationRequest as GotoDeclaration, DefinitionOptions, DefinitionRequest as GotoDefinition,
    DiagnosticOptions, DidChangeConfigurationNotification as DidChangeConfiguration,
    DidChangeTextDocumentNotification as DidChangeTextDocument,
    DidChangeWatchedFilesNotification as DidChangeWatchedFiles,
    DidChangeWorkspaceFoldersNotification as DidChangeWorkspaceFolders,
    DidCloseTextDocumentNotification as DidCloseTextDocument,
    DidCreateFilesNotification as DidCreateFiles, DidDeleteFilesNotification as DidDeleteFiles,
    DidOpenTextDocumentNotification as DidOpenTextDocument,
    DidRenameFilesNotification as DidRenameFiles,
    DidSaveTextDocumentNotification as DidSaveTextDocument, DocumentColorOptions,
    DocumentColorRequest as DocumentColor, DocumentDiagnosticReport, DocumentDiagnosticRequest,
    DocumentFormattingOptions, DocumentFormattingRequest as Formatting, DocumentHighlightOptions,
    DocumentHighlightRequest, DocumentLink, DocumentLinkOptions, DocumentLinkRequest,
    DocumentOnTypeFormattingOptions, DocumentOnTypeFormattingRequest as OnTypeFormatting,
    DocumentRangeFormattingOptions, DocumentRangeFormattingRequest as RangeFormatting,
    DocumentRangesFormattingRequest, DocumentSymbolOptions, DocumentSymbolRequest,
    FileOperationFilter, FileOperationPattern, FileOperationPatternKind,
    FileOperationRegistrationOptions, FoldingRangeOptions, FoldingRangeRequest, HoverRequest,
    ImplementationRegistrationOptions, ImplementationRequest as GotoImplementation, InlayHint,
    InlayHintOptions, InlayHintRequest, InlineCompletionOptions, InlineCompletionRequest,
    InlineValueOptions, InlineValueRequest, LinkedEditingRangeOptions,
    LinkedEditingRangeRequest as LinkedEditingRange, MonikerOptions, MonikerRequest, Notification,
    PrepareRenameRequest, ReferenceOptions, ReferencesRequest as References,
    RelatedFullDocumentDiagnosticReport, RenameOptions, RenameRequest as Rename, Request,
    SelectionRangeOptions, SelectionRangeRequest,
    SemanticTokensDeltaRequest as SemanticTokensFullDeltaRequest, SemanticTokensLegend,
    SemanticTokensOptions, SemanticTokensRangeRequest,
    SemanticTokensRequest as SemanticTokensFullRequest, SetTraceNotification as SetTrace,
    SignatureHelpOptions, SignatureHelpRequest, TextDocumentContentOptions,
    TextDocumentContentRequest, TextDocumentContentResult, TextDocumentSyncKind,
    TypeDefinitionRegistrationOptions, TypeDefinitionRequest as GotoTypeDefinition,
    TypeHierarchyOptions, TypeHierarchyPrepareRequest as TypeHierarchyPrepare,
    TypeHierarchySubtypesRequest as TypeHierarchySubtypes,
    TypeHierarchySupertypesRequest as TypeHierarchySupertypes,
    WillCreateFilesRequest as WillCreateFiles, WillDeleteFilesRequest as WillDeleteFiles,
    WillRenameFilesRequest as WillRenameFiles,
    WillSaveTextDocumentNotification as WillSaveTextDocument,
    WillSaveTextDocumentWaitUntilRequest as WillSaveWaitUntil, WorkDoneProgressOptions,
    WorkspaceDiagnosticReport, WorkspaceDiagnosticRequest, WorkspaceSymbol, WorkspaceSymbolOptions,
    WorkspaceSymbolRequest,
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
    ranges_formatting: DocumentRangesFormattingRequest => None,
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
    inline_completion: InlineCompletionRequest => None,
    workspace_symbol: WorkspaceSymbolRequest => None,
    will_save_wait_until: WillSaveWaitUntil => None,
    will_create_files: WillCreateFiles => None,
    will_rename_files: WillRenameFiles => None,
    will_delete_files: WillDeleteFiles => None,
    code_action: CodeActionRequest => None,
    code_lens: CodeLensRequest => None,
    document_link: DocumentLinkRequest => None,
    text_document_content: TextDocumentContentRequest => TextDocumentContentResult::default(),
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
) -> Result<DocumentDiagnosticReport, LspError> {
    Ok(
        DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(
            RelatedFullDocumentDiagnosticReport::default(),
        ),
    )
}

async fn workspace_diagnostic(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    _params: <WorkspaceDiagnosticRequest as Request>::Params,
    _ct: CancellationToken,
) -> Result<WorkspaceDiagnosticReport, LspError> {
    Ok(WorkspaceDiagnosticReport::default())
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
    const METHOD: lspf::types::LspRequestMethod<'static> =
        lspf::types::LspRequestMethod::Custom("catalog/extension");
    const MESSAGE_DIRECTION: lspf::types::MessageDirection =
        lspf::types::MessageDirection::ClientToServer;
}

enum CatalogNotice {}

impl Notification for CatalogNotice {
    type Params = Value;
    const METHOD: lspf::types::LspNotificationMethod<'static> =
        lspf::types::LspNotificationMethod::Custom("catalog/notice");
    const MESSAGE_DIRECTION: lspf::types::MessageDirection =
        lspf::types::MessageDirection::ClientToServer;
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
            token_types: vec!["keyword".to_string()],
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
            lspf::features::type_definition(TypeDefinitionRegistrationOptions::default()),
            type_definition,
        )
        .feature(
            lspf::features::implementation(ImplementationRegistrationOptions::default()),
            implementation,
        )
        .feature(
            lspf::features::references(ReferenceOptions {
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
            lspf::features::code_lens(CodeLensOptions {
                resolve_provider: None,
                ..CodeLensOptions::default()
            }),
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
                ranges_support: None,
            }),
            range_formatting,
        )
        .feature(
            lspf::features::ranges_formatting(DocumentRangeFormattingOptions {
                work_done_progress_options: work_done_options(),
                ranges_support: None,
            }),
            ranges_formatting,
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
            lspf::features::document_color(DocumentColorOptions::default()),
            document_color,
        )
        .feature(lspf::features::color_presentation(), color_presentation)
        .feature(
            lspf::features::folding_range(FoldingRangeOptions::default()),
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
            lspf::features::inline_completion(InlineCompletionOptions {
                work_done_progress_options: work_done_options(),
            }),
            inline_completion,
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
            lspf::features::text_document_content(TextDocumentContentOptions::new(vec![
                "git".to_string(),
            ])),
            text_document_content,
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

/// Register every feature, notification hook, and command the catalog
/// offers, with fixed options, and return the built server.
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

// --- The vendored metaModel --------------------------------------------------

/// The official LSP metaModel, vendored verbatim from
/// `_specifications/lsp/3.18/metaModel/metaModel.json` on the `gh-pages`
/// branch of `microsoft/language-server-protocol` (commit
/// `b7f5132c95261c0898ae5124e7a91707abc48fcd`, SHA-256
/// `caae8df639a4248520a3f589fd72945365e9d8ebca5baf564161a515430d9d41`),
/// copyright Microsoft Corporation under the MIT licence. Refresh it by
/// downloading that path again, never by editing it here: the recorded hash
/// is what makes it evidence of the spec rather than of our reading of it.
const META_MODEL: &str = include_str!("fixtures/lsp_meta_model_3_18_0.json");

/// The metaModel version this guardrail is written against. Because the
/// vendored model *is* the 3.18.0 release, "every field the spec marks
/// available at or before 3.18.0" is exactly "every field in the fixture" —
/// no version arithmetic is needed, and
/// [`the_vendored_meta_model_is_the_3_18_0_release`] keeps that equivalence
/// from quietly lapsing when the fixture is refreshed.
const META_MODEL_VERSION: &str = "3.18.0";

/// A metaModel structure whose every property states that the server supports
/// something, paired with the dotted path its fields carry in the capability
/// object. `path` is empty for the root record.
struct CapabilityRecord {
    path: &'static str,
    structure: &'static str,
}

/// The records the capability-field walk covers. The rule for membership is
/// that every property is itself a switch saying the server has a capability;
/// a record of *how* a provider behaves — `CompletionOptions`'
/// trigger characters, `SemanticTokensOptions`' legend — is option detail and
/// stays out, because a server declining to set one is a configuration choice
/// rather than a gap against the spec.
const CAPABILITY_RECORDS: [CapabilityRecord; 4] = [
    CapabilityRecord {
        path: "",
        structure: "ServerCapabilities",
    },
    CapabilityRecord {
        path: "textDocumentSync",
        structure: "TextDocumentSyncOptions",
    },
    CapabilityRecord {
        path: "workspace",
        structure: "WorkspaceOptions",
    },
    CapabilityRecord {
        path: "workspace.fileOperations",
        structure: "FileOperationOptions",
    },
];

/// Capability fields LSP 3.18 defines that the catalog cannot yet produce.
/// Each entry is a commitment rather than an exemption: the work named beside
/// it deletes its line, and until then the guardrail pins the gap so it cannot
/// grow silently.
const UNPRODUCIBLE_CAPABILITY_FIELDS: [&str; 2] = [
    // The server-defined escape hatch. It carries no protocol meaning of its
    // own, lspf exposes no way to set it, and no ticket plans one.
    "experimental",
    // Notebook document sync, issue #252.
    "notebookDocumentSync",
];

fn meta_model() -> Value {
    serde_json::from_str(META_MODEL).expect("the vendored metaModel is valid JSON")
}

/// Every server capability field the metaModel defines, as a dotted path,
/// mapped to whether the spec marks that field proposed.
fn meta_model_capability_fields(model: &Value) -> BTreeMap<String, bool> {
    let structures = model["structures"]
        .as_array()
        .expect("the metaModel lists structures");
    let mut fields = BTreeMap::new();
    for record in CAPABILITY_RECORDS {
        let structure = structures
            .iter()
            .find(|structure| structure["name"] == json!(record.structure))
            .unwrap_or_else(|| panic!("the metaModel defines {}", record.structure));
        let properties = structure["properties"]
            .as_array()
            .unwrap_or_else(|| panic!("{} lists properties", record.structure));
        for property in properties {
            let name = property["name"]
                .as_str()
                .unwrap_or_else(|| panic!("every {} property is named", record.structure));
            fields.insert(
                capability_path(record.path, name),
                property["proposed"] == json!(true),
            );
        }
    }
    fields
}

/// The capability field paths an initialize response actually carries, walked
/// over the same records as [`meta_model_capability_fields`] so the two sets
/// are directly comparable.
fn advertised_capability_fields(wire: &str) -> BTreeSet<String> {
    let response: Value = serde_json::from_str(wire).expect("the initialize response is JSON");
    let capabilities = &response["capabilities"];
    assert!(
        capabilities.is_object(),
        "the initialize response carries a capability object: {wire}"
    );

    let mut fields = BTreeSet::new();
    for record in CAPABILITY_RECORDS {
        // An absent record advertises no fields, and a record the spec models
        // as a union may arrive in a form that names none — `textDocumentSync`
        // is either sync options or a bare sync kind.
        let Some(present) = record
            .path
            .split('.')
            .filter(|segment| !segment.is_empty())
            .try_fold(capabilities, |value, segment| value.get(segment))
            .and_then(Value::as_object)
        else {
            continue;
        };
        for name in present.keys() {
            fields.insert(capability_path(record.path, name));
        }
    }
    fields
}

fn capability_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

fn capability_field_set(fields: &[&str]) -> BTreeSet<String> {
    fields.iter().map(|field| (*field).to_string()).collect()
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

/// The vendored fixture is the 3.18.0 release, which is what lets the
/// guardrails below read "every field in the fixture" as "every field the
/// spec makes available at or before 3.18.0". Refreshing the fixture to a
/// later metaModel must be a deliberate change that revisits them.
#[test]
fn the_vendored_meta_model_is_the_3_18_0_release() {
    assert_eq!(
        meta_model()["metaData"]["version"],
        json!(META_MODEL_VERSION),
        "the vendored metaModel must stay the release this guardrail is written against"
    );
}

/// Every server capability field LSP 3.18 defines and does not mark proposed
/// must be producible by the catalog. The exceptions are pinned by name in
/// [`UNPRODUCIBLE_CAPABILITY_FIELDS`], so a spec field the catalog cannot
/// advertise is a listed, attributed gap rather than an oversight.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalog_produces_every_stable_capability_field_the_spec_defines() {
    let (wire, _) = initialize_wire(full_catalog()).await;
    let advertised = advertised_capability_fields(&wire);

    let missing: BTreeSet<String> = meta_model_capability_fields(&meta_model())
        .into_iter()
        .filter(|(field, proposed)| !proposed && !advertised.contains(field))
        .map(|(field, _)| field)
        .collect();
    assert_eq!(
        missing,
        capability_field_set(&UNPRODUCIBLE_CAPABILITY_FIELDS),
        "the stable {META_MODEL_VERSION} capability fields the catalog cannot produce must be \
         exactly the pinned gaps; landing one deletes its line, losing one adds a regression"
    );
}

/// The catalog advertises nothing the spec does not define, and nothing the
/// spec marks proposed. Both halves read the fixture, so a draft field that
/// nobody thought to forbid is still caught by the first one.
///
/// The proposed half is inert against this fixture: the 3.18.0 metaModel
/// marks no capability field proposed, so it can only start failing once the
/// fixture is refreshed to a metaModel that carries proposals. It is kept
/// because that refresh is exactly when a proposal could slip in unnoticed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_proposed_or_unspecified_field_leaks_into_the_catalog() {
    let (wire, _) = initialize_wire(full_catalog()).await;
    let advertised = advertised_capability_fields(&wire);
    let specified = meta_model_capability_fields(&meta_model());

    let proposed: Vec<&String> = advertised
        .iter()
        .filter(|field| specified.get(*field) == Some(&true))
        .collect();
    assert!(
        proposed.is_empty(),
        "the catalog advertises fields LSP {META_MODEL_VERSION} marks proposed: {proposed:?}"
    );

    let unspecified: Vec<&String> = advertised
        .iter()
        .filter(|field| !specified.contains_key(*field))
        .collect();
    assert!(
        unspecified.is_empty(),
        "the catalog advertises fields LSP {META_MODEL_VERSION} does not define: {unspecified:?}"
    );
}

/// The default catalog — no registrations at all — advertises exactly the
/// protocol-owned fields: the negotiated position encoding, document sync,
/// and workspace-folder support. Stating the whole set rather than naming
/// forbidden fields keeps every capability a feature owns opt-in, including
/// ones no exclusion list would have thought to mention, and is what still
/// enforces ADR 0024's default-catalog boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_default_catalog_advertises_only_protocol_owned_fields() {
    let (wire, _) = initialize_wire(
        Server::builder(AppState)
            .build()
            .expect("an empty server builds"),
    )
    .await;

    let advertised = advertised_capability_fields(&wire);
    let protocol_owned = capability_field_set(&[
        "positionEncoding",
        "textDocumentSync",
        "workspace",
        "workspace.workspaceFolders",
    ]);

    assert_eq!(
        advertised, protocol_owned,
        "a server that registers nothing advertises only what the protocol itself owns"
    );

    // No `textDocumentSync.*` field is expected above because with no document
    // hook registered the sync capability takes the union's other arm: a bare
    // sync kind, which names no fields at all.
    let parsed: Value = serde_json::from_str(&wire).unwrap();
    assert_eq!(
        parsed["capabilities"]["textDocumentSync"],
        json!(TextDocumentSyncKind::Incremental),
        "the default sync capability is the bare incremental kind"
    );
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

/// Enabling the `proposed` Cargo feature must not move the catalog boundary
/// (issue #108): the full-catalog capability bytes stay pinned to the fixture.
/// The feature now exposes only compatibility aliases for outgoing request
/// types, so the Router catalog cannot change.
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
}
