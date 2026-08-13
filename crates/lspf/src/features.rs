//! Standard LSP feature descriptors (ADR 0017).
//!
//! Each `features::*` constructor returns a descriptor implementing one of the
//! sealed traits [`FeatureSpec`] (request features) or
//! [`NotificationFeatureSpec`] (notification features). A descriptor fixes
//! three things at once: the `lsp_types` request or notification marker that
//! names the wire method and its typed parameters (and result, for requests),
//! the descriptor's public options, and the single deterministic contribution
//! the feature makes to the capability catalog.
//!
//! Both traits are sealed: downstream crates use these descriptors but cannot
//! implement either trait, so they cannot present a pseudo-standard feature
//! whose capability fragment lspf does not know how to merge. Custom methods
//! use [`request`](crate::ServerBuilder::request) and
//! [`notification`](crate::ServerBuilder::notification) instead and advertise
//! no capability.

use lsp_types::notification::{
    DidChangeWatchedFiles, DidCreateFiles, DidDeleteFiles, DidRenameFiles, Notification,
};
use lsp_types::request::{
    CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
    CodeActionRequest, CodeActionResolveRequest, CodeLensRequest, CodeLensResolve,
    ColorPresentationRequest, Completion, DocumentColor, DocumentDiagnosticRequest,
    DocumentHighlightRequest, DocumentLinkRequest, DocumentLinkResolve, DocumentSymbolRequest,
    FoldingRangeRequest, Formatting, GotoDeclaration, GotoDefinition, GotoImplementation,
    GotoTypeDefinition, HoverRequest, InlayHintRequest, InlayHintResolveRequest,
    InlineValueRequest, LinkedEditingRange, MonikerRequest, OnTypeFormatting, PrepareRenameRequest,
    RangeFormatting, References, Rename, Request, ResolveCompletionItem, SelectionRangeRequest,
    SemanticTokensFullDeltaRequest, SemanticTokensFullRequest, SemanticTokensRangeRequest,
    SignatureHelpRequest, TypeHierarchyPrepare, TypeHierarchySubtypes, TypeHierarchySupertypes,
    WillCreateFiles, WillDeleteFiles, WillRenameFiles, WillSaveWaitUntil,
    WorkspaceDiagnosticRequest, WorkspaceSymbolRequest, WorkspaceSymbolResolve,
};
use lsp_types::{
    CallHierarchyOptions, CodeActionOptions, CodeLensOptions, ColorProviderOptions,
    CompletionOptions, DeclarationOptions, DefinitionOptions, DiagnosticOptions,
    DocumentFormattingOptions, DocumentHighlightOptions, DocumentLinkOptions,
    DocumentOnTypeFormattingOptions, DocumentRangeFormattingOptions, DocumentSymbolOptions,
    FileOperationRegistrationOptions, FoldingProviderOptions, InlayHintOptions, InlineValueOptions,
    LinkedEditingRangeOptions, MonikerOptions, ReferencesOptions, RenameOptions,
    SelectionRangeOptions, SemanticTokensOptions, SignatureHelpOptions,
    StaticTextDocumentRegistrationOptions, TypeHierarchyOptions, WorkspaceSymbolOptions,
};

use crate::capability::CapabilityBuilder;
use crate::error::BuildError;

pub(crate) mod sealed {
    use crate::capability::CapabilityBuilder;
    use crate::error::BuildError;

    /// The in-crate half of [`FeatureSpec`](super::FeatureSpec). Being only
    /// `pub(crate)` it both seals the public trait against downstream
    /// implementations and keeps the internal [`CapabilityBuilder`] out of the
    /// public API.
    ///
    /// `contribute` names the crate-private `CapabilityBuilder`, which the
    /// public `FeatureSpec` supertrait bound technically makes reachable. The
    /// leak is inert — `Sealed` cannot be implemented and `CapabilityBuilder`
    /// cannot be named or constructed outside the crate — so the lint is
    /// silenced deliberately.
    #[allow(private_interfaces)]
    pub trait Sealed {
        /// Record this feature's contribution to the capability catalog, or
        /// return a [`BuildError`] if it conflicts with an existing one.
        fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError>;
    }
}

/// The sealed descriptor contract for a standard LSP feature (ADR 0017).
///
/// [`Marker`](Self::Marker) is the `lsp_types` request marker fixing the wire
/// method and the typed parameter and result types dispatch uses. The
/// capability contribution lives on the sealed in-crate supertrait, so this
/// trait is usable but not implementable downstream. Implemented only by the
/// descriptors returned from this module.
pub trait FeatureSpec: sealed::Sealed {
    /// The request marker this feature dispatches, fixing its method and its
    /// parameter and result types.
    type Marker: Request;
}

/// The sealed descriptor contract for a standard LSP notification feature
/// (ADR 0017).
///
/// The notification counterpart of [`FeatureSpec`]: [`Marker`](Self::Marker)
/// is the `lsp_types` notification marker fixing the wire method and the typed
/// parameter type dispatch uses. Registered through
/// [`ServerBuilder::feature_notification`](crate::ServerBuilder::feature_notification).
/// Implemented only by the descriptors returned from this module.
pub trait NotificationFeatureSpec: sealed::Sealed {
    /// The notification marker this feature dispatches, fixing its method and
    /// its parameter type.
    type Marker: Notification;
}

/// The `textDocument/willSaveWaitUntil` request feature descriptor.
pub struct WillSaveWaitUntilFeature(());

/// Describe the typed pre-save edit request. Its registration contributes
/// `willSaveWaitUntil: true` to the effective text-document sync options.
pub fn will_save_wait_until() -> WillSaveWaitUntilFeature {
    WillSaveWaitUntilFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for WillSaveWaitUntilFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_will_save_wait_until();
        Ok(())
    }
}
impl FeatureSpec for WillSaveWaitUntilFeature {
    type Marker = WillSaveWaitUntil;
}

/// The `textDocument/hover` feature descriptor. Construct it with [`hover`].
pub struct HoverFeature(());

/// Describe the standard hover feature: it dispatches
/// [`HoverParams`](lsp_types::HoverParams), returns
/// [`Option<Hover>`](lsp_types::Hover), and advertises `hoverProvider`.
pub fn hover() -> HoverFeature {
    HoverFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for HoverFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_hover()
    }
}
impl FeatureSpec for HoverFeature {
    type Marker = HoverRequest;
}

/// The `textDocument/signatureHelp` feature descriptor. Construct it with
/// [`signature_help`].
pub struct SignatureHelpFeature {
    options: SignatureHelpOptions,
}

/// Describe the standard signature-help feature: it dispatches the lsp-types
/// [`SignatureHelpRequest`] marker — typed
/// [`SignatureHelpParams`](lsp_types::SignatureHelpParams) in, optional
/// [`SignatureHelp`](lsp_types::SignatureHelp) out — and advertises the
/// supplied [`SignatureHelpOptions`] as `signatureHelpProvider`.
pub fn signature_help(options: SignatureHelpOptions) -> SignatureHelpFeature {
    SignatureHelpFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for SignatureHelpFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_signature_help(self.options.clone())
    }
}
impl FeatureSpec for SignatureHelpFeature {
    type Marker = SignatureHelpRequest;
}

/// The `textDocument/declaration` feature descriptor. Construct it with
/// [`declaration`].
pub struct DeclarationFeature {
    options: DeclarationOptions,
}

/// Describe the standard declaration feature: it dispatches the lsp-types
/// [`GotoDeclaration`] marker and advertises the supplied [`DeclarationOptions`]
/// as the singular `declarationProvider` capability.
pub fn declaration(options: DeclarationOptions) -> DeclarationFeature {
    DeclarationFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for DeclarationFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_declaration(self.options.clone())
    }
}
impl FeatureSpec for DeclarationFeature {
    type Marker = GotoDeclaration;
}

/// The `textDocument/definition` feature descriptor. Construct it with
/// [`definition`].
pub struct DefinitionFeature {
    options: DefinitionOptions,
}

/// Describe the standard definition feature: it dispatches the lsp-types
/// [`GotoDefinition`] marker and advertises the supplied [`DefinitionOptions`]
/// as the singular `definitionProvider` capability.
pub fn definition(options: DefinitionOptions) -> DefinitionFeature {
    DefinitionFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for DefinitionFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_definition(self.options.clone())
    }
}
impl FeatureSpec for DefinitionFeature {
    type Marker = GotoDefinition;
}

/// The `textDocument/typeDefinition` feature descriptor. Construct it with
/// [`type_definition`].
pub struct TypeDefinitionFeature {
    options: StaticTextDocumentRegistrationOptions,
}

/// Describe the standard type-definition feature: it dispatches the lsp-types
/// [`GotoTypeDefinition`] marker and advertises the supplied
/// [`StaticTextDocumentRegistrationOptions`] as the singular
/// `typeDefinitionProvider` capability.
pub fn type_definition(options: StaticTextDocumentRegistrationOptions) -> TypeDefinitionFeature {
    TypeDefinitionFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for TypeDefinitionFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_type_definition(self.options.clone())
    }
}
impl FeatureSpec for TypeDefinitionFeature {
    type Marker = GotoTypeDefinition;
}

/// The `textDocument/implementation` feature descriptor. Construct it with
/// [`implementation`].
pub struct ImplementationFeature {
    options: StaticTextDocumentRegistrationOptions,
}

/// Describe the standard implementation feature: it dispatches the lsp-types
/// [`GotoImplementation`] marker and advertises the supplied
/// [`StaticTextDocumentRegistrationOptions`] as the singular
/// `implementationProvider` capability.
pub fn implementation(options: StaticTextDocumentRegistrationOptions) -> ImplementationFeature {
    ImplementationFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for ImplementationFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_implementation(self.options.clone())
    }
}
impl FeatureSpec for ImplementationFeature {
    type Marker = GotoImplementation;
}

/// The `textDocument/references` feature descriptor. Construct it with
/// [`references`].
pub struct ReferencesFeature {
    options: ReferencesOptions,
}

/// Describe the standard references feature: it dispatches the lsp-types
/// [`References`] marker and advertises the supplied [`ReferencesOptions`] as
/// the singular `referencesProvider` capability.
pub fn references(options: ReferencesOptions) -> ReferencesFeature {
    ReferencesFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for ReferencesFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_references(self.options.clone())
    }
}
impl FeatureSpec for ReferencesFeature {
    type Marker = References;
}

/// The `textDocument/documentHighlight` feature descriptor. Construct it with
/// [`document_highlight`].
pub struct DocumentHighlightFeature {
    options: DocumentHighlightOptions,
}

/// Describe the standard document-highlight feature: it dispatches the
/// lsp-types [`DocumentHighlightRequest`] marker and advertises the supplied
/// [`DocumentHighlightOptions`] as the singular `documentHighlightProvider`
/// capability.
pub fn document_highlight(options: DocumentHighlightOptions) -> DocumentHighlightFeature {
    DocumentHighlightFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for DocumentHighlightFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_document_highlight(self.options.clone())
    }
}
impl FeatureSpec for DocumentHighlightFeature {
    type Marker = DocumentHighlightRequest;
}

/// The `textDocument/documentSymbol` feature descriptor. Construct it with
/// [`document_symbol`].
pub struct DocumentSymbolFeature {
    options: DocumentSymbolOptions,
}

/// Describe the standard document-symbol feature: it dispatches the lsp-types
/// [`DocumentSymbolRequest`] marker and advertises the supplied
/// [`DocumentSymbolOptions`] as the singular `documentSymbolProvider`
/// capability.
pub fn document_symbol(options: DocumentSymbolOptions) -> DocumentSymbolFeature {
    DocumentSymbolFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for DocumentSymbolFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_document_symbol(self.options.clone())
    }
}
impl FeatureSpec for DocumentSymbolFeature {
    type Marker = DocumentSymbolRequest;
}

/// The `textDocument/linkedEditingRange` feature descriptor. Construct it with
/// [`linked_editing_range`].
pub struct LinkedEditingRangeFeature {
    options: LinkedEditingRangeOptions,
}

/// Describe the standard linked-editing-range feature: it dispatches the
/// lsp-types [`LinkedEditingRange`] marker and advertises the supplied
/// [`LinkedEditingRangeOptions`] as the singular `linkedEditingRangeProvider`
/// capability.
pub fn linked_editing_range(options: LinkedEditingRangeOptions) -> LinkedEditingRangeFeature {
    LinkedEditingRangeFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for LinkedEditingRangeFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_linked_editing_range(self.options.clone())
    }
}
impl FeatureSpec for LinkedEditingRangeFeature {
    type Marker = LinkedEditingRange;
}

/// The `textDocument/moniker` feature descriptor. Construct it with
/// [`moniker`].
pub struct MonikerFeature {
    options: MonikerOptions,
}

/// Describe the standard moniker feature: it dispatches the lsp-types
/// [`MonikerRequest`] marker and advertises the supplied [`MonikerOptions`] as
/// the singular `monikerProvider` capability.
pub fn moniker(options: MonikerOptions) -> MonikerFeature {
    MonikerFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for MonikerFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_moniker(self.options.clone())
    }
}
impl FeatureSpec for MonikerFeature {
    type Marker = MonikerRequest;
}

/// The `textDocument/formatting` feature descriptor.
pub struct DocumentFormattingFeature {
    options: DocumentFormattingOptions,
}

/// Describe whole-document formatting and its provider options.
pub fn document_formatting(options: DocumentFormattingOptions) -> DocumentFormattingFeature {
    DocumentFormattingFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for DocumentFormattingFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_document_formatting(self.options.clone())
    }
}
impl FeatureSpec for DocumentFormattingFeature {
    type Marker = Formatting;
}

/// The `textDocument/rangeFormatting` feature descriptor.
pub struct RangeFormattingFeature {
    options: DocumentRangeFormattingOptions,
}

/// Describe range formatting and its provider options.
pub fn range_formatting(options: DocumentRangeFormattingOptions) -> RangeFormattingFeature {
    RangeFormattingFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for RangeFormattingFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_range_formatting(self.options.clone())
    }
}
impl FeatureSpec for RangeFormattingFeature {
    type Marker = RangeFormatting;
}

/// The `textDocument/onTypeFormatting` feature descriptor.
pub struct OnTypeFormattingFeature {
    options: DocumentOnTypeFormattingOptions,
}

/// Describe on-type formatting and its trigger-character options.
pub fn on_type_formatting(options: DocumentOnTypeFormattingOptions) -> OnTypeFormattingFeature {
    OnTypeFormattingFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for OnTypeFormattingFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_on_type_formatting(self.options.clone())
    }
}
impl FeatureSpec for OnTypeFormattingFeature {
    type Marker = OnTypeFormatting;
}

/// The `textDocument/documentColor` feature descriptor.
pub struct DocumentColorFeature {
    options: ColorProviderOptions,
}

/// Describe document-color discovery and the shared color provider.
pub fn document_color(options: ColorProviderOptions) -> DocumentColorFeature {
    DocumentColorFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for DocumentColorFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_document_color(self.options.clone())
    }
}
impl FeatureSpec for DocumentColorFeature {
    type Marker = DocumentColor;
}

/// The `textDocument/colorPresentation` feature descriptor.
pub struct ColorPresentationFeature(());

/// Describe color presentation, the subordinate route of `colorProvider`.
pub fn color_presentation() -> ColorPresentationFeature {
    ColorPresentationFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for ColorPresentationFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_color_presentation();
        Ok(())
    }
}
impl FeatureSpec for ColorPresentationFeature {
    type Marker = ColorPresentationRequest;
}

/// The `textDocument/foldingRange` feature descriptor.
pub struct FoldingRangeFeature {
    options: FoldingProviderOptions,
}

/// Describe folding-range discovery and its provider options.
pub fn folding_range(options: FoldingProviderOptions) -> FoldingRangeFeature {
    FoldingRangeFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for FoldingRangeFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_folding_range(self.options.clone())
    }
}
impl FeatureSpec for FoldingRangeFeature {
    type Marker = FoldingRangeRequest;
}

/// The `textDocument/selectionRange` feature descriptor.
pub struct SelectionRangeFeature {
    options: SelectionRangeOptions,
}

/// Describe selection-range discovery and its provider options.
pub fn selection_range(options: SelectionRangeOptions) -> SelectionRangeFeature {
    SelectionRangeFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for SelectionRangeFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_selection_range(self.options.clone())
    }
}
impl FeatureSpec for SelectionRangeFeature {
    type Marker = SelectionRangeRequest;
}

/// The `textDocument/inlineValue` feature descriptor.
pub struct InlineValueFeature {
    options: InlineValueOptions,
}

/// Describe inline-value calculation and its provider options.
pub fn inline_value(options: InlineValueOptions) -> InlineValueFeature {
    InlineValueFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for InlineValueFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_inline_value(self.options.clone())
    }
}
impl FeatureSpec for InlineValueFeature {
    type Marker = InlineValueRequest;
}

/// The `textDocument/prepareCallHierarchy` feature descriptor.
pub struct CallHierarchyPrepareFeature {
    options: CallHierarchyOptions,
}

/// Describe the call-hierarchy prepare feature and its single provider
/// capability. Incoming and outgoing call routes depend on this feature.
pub fn call_hierarchy_prepare(options: CallHierarchyOptions) -> CallHierarchyPrepareFeature {
    CallHierarchyPrepareFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for CallHierarchyPrepareFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_call_hierarchy(self.options)
    }
}
impl FeatureSpec for CallHierarchyPrepareFeature {
    type Marker = CallHierarchyPrepare;
}

/// The `callHierarchy/incomingCalls` feature descriptor.
pub struct CallHierarchyIncomingCallsFeature(());

/// Describe the incoming-call route. It contributes membership to the
/// call-hierarchy family without emitting a second provider capability.
pub fn call_hierarchy_incoming_calls() -> CallHierarchyIncomingCallsFeature {
    CallHierarchyIncomingCallsFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for CallHierarchyIncomingCallsFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_call_hierarchy_incoming_calls();
        Ok(())
    }
}
impl FeatureSpec for CallHierarchyIncomingCallsFeature {
    type Marker = CallHierarchyIncomingCalls;
}

/// The `callHierarchy/outgoingCalls` feature descriptor.
pub struct CallHierarchyOutgoingCallsFeature(());

/// Describe the outgoing-call route. It contributes membership to the
/// call-hierarchy family without emitting a second provider capability.
pub fn call_hierarchy_outgoing_calls() -> CallHierarchyOutgoingCallsFeature {
    CallHierarchyOutgoingCallsFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for CallHierarchyOutgoingCallsFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_call_hierarchy_outgoing_calls();
        Ok(())
    }
}
impl FeatureSpec for CallHierarchyOutgoingCallsFeature {
    type Marker = CallHierarchyOutgoingCalls;
}

/// The `textDocument/prepareTypeHierarchy` feature descriptor.
pub struct TypeHierarchyPrepareFeature {
    options: TypeHierarchyOptions,
}

/// Describe the type-hierarchy prepare feature and its single provider
/// capability. Supertype and subtype routes depend on this feature.
pub fn type_hierarchy_prepare(options: TypeHierarchyOptions) -> TypeHierarchyPrepareFeature {
    TypeHierarchyPrepareFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for TypeHierarchyPrepareFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_type_hierarchy(self.options.clone())
    }
}
impl FeatureSpec for TypeHierarchyPrepareFeature {
    type Marker = TypeHierarchyPrepare;
}

/// The `typeHierarchy/supertypes` feature descriptor.
pub struct TypeHierarchySupertypesFeature(());

/// Describe the supertype route without emitting another prepare capability.
pub fn type_hierarchy_supertypes() -> TypeHierarchySupertypesFeature {
    TypeHierarchySupertypesFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for TypeHierarchySupertypesFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_type_hierarchy_supertypes();
        Ok(())
    }
}
impl FeatureSpec for TypeHierarchySupertypesFeature {
    type Marker = TypeHierarchySupertypes;
}

/// The `typeHierarchy/subtypes` feature descriptor.
pub struct TypeHierarchySubtypesFeature(());

/// Describe the subtype route without emitting another prepare capability.
pub fn type_hierarchy_subtypes() -> TypeHierarchySubtypesFeature {
    TypeHierarchySubtypesFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for TypeHierarchySubtypesFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_type_hierarchy_subtypes();
        Ok(())
    }
}
impl FeatureSpec for TypeHierarchySubtypesFeature {
    type Marker = TypeHierarchySubtypes;
}

/// The `textDocument/semanticTokens/full` feature descriptor.
pub struct SemanticTokensFullFeature {
    options: SemanticTokensOptions,
}

/// Describe full-document semantic tokens. All semantic-token descriptors in
/// one family must agree on their legend and shared options.
pub fn semantic_tokens_full(options: SemanticTokensOptions) -> SemanticTokensFullFeature {
    SemanticTokensFullFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for SemanticTokensFullFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_semantic_tokens_full(self.options.clone())
    }
}
impl FeatureSpec for SemanticTokensFullFeature {
    type Marker = SemanticTokensFullRequest;
}

/// The `textDocument/semanticTokens/full/delta` feature descriptor.
pub struct SemanticTokensFullDeltaFeature {
    options: SemanticTokensOptions,
}

/// Describe semantic-token deltas, which depend on the full-document route.
pub fn semantic_tokens_full_delta(
    options: SemanticTokensOptions,
) -> SemanticTokensFullDeltaFeature {
    SemanticTokensFullDeltaFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for SemanticTokensFullDeltaFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_semantic_tokens_full_delta(self.options.clone())
    }
}
impl FeatureSpec for SemanticTokensFullDeltaFeature {
    type Marker = SemanticTokensFullDeltaRequest;
}

/// The `textDocument/semanticTokens/range` feature descriptor.
pub struct SemanticTokensRangeFeature {
    options: SemanticTokensOptions,
}

/// Describe range semantic tokens and merge them into the family's one
/// provider capability.
pub fn semantic_tokens_range(options: SemanticTokensOptions) -> SemanticTokensRangeFeature {
    SemanticTokensRangeFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for SemanticTokensRangeFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_semantic_tokens_range(self.options.clone())
    }
}
impl FeatureSpec for SemanticTokensRangeFeature {
    type Marker = SemanticTokensRangeRequest;
}

/// The `textDocument/completion` feature descriptor. Construct it with
/// [`completion`].
pub struct CompletionFeature {
    options: CompletionOptions,
}

/// Describe the standard completion feature: it dispatches the lsp-types
/// [`Completion`] marker and advertises the supplied [`CompletionOptions`] as
/// `completionProvider`.
pub fn completion(options: CompletionOptions) -> CompletionFeature {
    CompletionFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for CompletionFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_completion(self.options.clone())
    }
}
impl FeatureSpec for CompletionFeature {
    type Marker = Completion;
}

/// The `completionItem/resolve` feature descriptor. Construct it with
/// [`completion_resolve`].
pub struct CompletionResolveFeature(());

/// Describe the standard completion-item resolve feature: it dispatches the
/// lsp-types [`ResolveCompletionItem`] marker — a typed
/// [`CompletionItem`](lsp_types::CompletionItem) in and out — and augments
/// the completion family's capability with `resolveProvider`.
///
/// Resolve is a dependent feature: registering it without the base
/// [`completion`] feature fails validation with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability)
/// rather than advertising a dangling `resolveProvider`.
pub fn completion_resolve() -> CompletionResolveFeature {
    CompletionResolveFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for CompletionResolveFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_completion_resolve();
        Ok(())
    }
}
impl FeatureSpec for CompletionResolveFeature {
    type Marker = ResolveCompletionItem;
}

/// The `textDocument/diagnostic` feature descriptor. Construct it with
/// [`document_diagnostic`].
pub struct DocumentDiagnosticFeature {
    options: DiagnosticOptions,
}

/// Describe document diagnostics with the shared pull-diagnostics provider
/// options and the exact [`DocumentDiagnosticRequest`] wire contract.
pub fn document_diagnostic(options: DiagnosticOptions) -> DocumentDiagnosticFeature {
    DocumentDiagnosticFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for DocumentDiagnosticFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_diagnostics(self.options.clone())
    }
}
impl FeatureSpec for DocumentDiagnosticFeature {
    type Marker = DocumentDiagnosticRequest;
}

/// The `workspace/diagnostic` feature descriptor. Construct it with
/// [`workspace_diagnostic`].
pub struct WorkspaceDiagnosticFeature {
    options: DiagnosticOptions,
}

/// Describe workspace diagnostics with the shared pull-diagnostics provider
/// options and the exact [`WorkspaceDiagnosticRequest`] wire contract.
pub fn workspace_diagnostic(options: DiagnosticOptions) -> WorkspaceDiagnosticFeature {
    WorkspaceDiagnosticFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for WorkspaceDiagnosticFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_diagnostics(self.options.clone())
    }
}
impl FeatureSpec for WorkspaceDiagnosticFeature {
    type Marker = WorkspaceDiagnosticRequest;
}

/// The `workspace/symbol` feature descriptor. Construct it with
/// [`workspace_symbol`].
pub struct WorkspaceSymbolFeature {
    options: WorkspaceSymbolOptions,
}

/// Describe the standard workspace-symbol feature: it dispatches the
/// lsp-types [`WorkspaceSymbolRequest`] marker and advertises the supplied
/// [`WorkspaceSymbolOptions`] as the base of the `workspaceSymbolProvider`
/// capability family.
pub fn workspace_symbol(options: WorkspaceSymbolOptions) -> WorkspaceSymbolFeature {
    WorkspaceSymbolFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for WorkspaceSymbolFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_workspace_symbol(self.options.clone())
    }
}
impl FeatureSpec for WorkspaceSymbolFeature {
    type Marker = WorkspaceSymbolRequest;
}

/// The `workspaceSymbol/resolve` feature descriptor. Construct it with
/// [`workspace_symbol_resolve`].
pub struct WorkspaceSymbolResolveFeature(());

/// Describe the standard workspace-symbol resolve feature: it dispatches the
/// lsp-types [`WorkspaceSymbolResolve`] marker — a typed
/// [`WorkspaceSymbol`](lsp_types::WorkspaceSymbol) in and out — and augments
/// the workspace-symbol family's capability with `resolveProvider`.
///
/// Resolve is a dependent feature: registering it without the base
/// [`workspace_symbol`] feature fails validation with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability)
/// rather than advertising a dangling `resolveProvider`.
pub fn workspace_symbol_resolve() -> WorkspaceSymbolResolveFeature {
    WorkspaceSymbolResolveFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for WorkspaceSymbolResolveFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_workspace_symbol_resolve();
        Ok(())
    }
}
impl FeatureSpec for WorkspaceSymbolResolveFeature {
    type Marker = WorkspaceSymbolResolve;
}

/// The `textDocument/rename` feature descriptor. Construct it with [`rename`].
pub struct RenameFeature {
    options: RenameOptions,
}

/// Describe the standard rename feature: it dispatches the lsp-types
/// [`Rename`] marker and advertises the supplied [`RenameOptions`] as the base
/// of the `renameProvider` capability family.
pub fn rename(options: RenameOptions) -> RenameFeature {
    RenameFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for RenameFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_rename(self.options.clone())
    }
}
impl FeatureSpec for RenameFeature {
    type Marker = Rename;
}

/// The `textDocument/prepareRename` feature descriptor. Construct it with
/// [`prepare_rename`].
pub struct PrepareRenameFeature(());

/// Describe the standard prepare-rename feature: it dispatches the lsp-types
/// [`PrepareRenameRequest`] marker and augments the rename family's capability
/// with `prepareProvider`.
///
/// Prepare rename is a dependent feature: registering it without the base
/// [`rename`] feature fails validation with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability)
/// rather than advertising a dangling `prepareProvider`.
pub fn prepare_rename() -> PrepareRenameFeature {
    PrepareRenameFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for PrepareRenameFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_prepare_rename();
        Ok(())
    }
}
impl FeatureSpec for PrepareRenameFeature {
    type Marker = PrepareRenameRequest;
}

/// The `textDocument/codeAction` feature descriptor. Construct it with
/// [`code_action`].
pub struct CodeActionFeature {
    options: CodeActionOptions,
}

/// Describe the standard code-action feature: it dispatches the lsp-types
/// [`CodeActionRequest`] marker and advertises the supplied
/// [`CodeActionOptions`] as the base of the `codeActionProvider` capability
/// family.
pub fn code_action(options: CodeActionOptions) -> CodeActionFeature {
    CodeActionFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for CodeActionFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_code_action(self.options.clone())
    }
}
impl FeatureSpec for CodeActionFeature {
    type Marker = CodeActionRequest;
}

/// The `codeAction/resolve` feature descriptor. Construct it with
/// [`code_action_resolve`].
pub struct CodeActionResolveFeature(());

/// Describe the standard code-action resolve feature: it dispatches the
/// lsp-types [`CodeActionResolveRequest`] marker — a typed
/// [`CodeAction`](lsp_types::CodeAction) in and out — and augments the
/// code-action family's capability with `resolveProvider`.
///
/// Resolve is a dependent feature: registering it without the base
/// [`code_action`] feature fails validation with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability)
/// rather than advertising a dangling `resolveProvider`.
pub fn code_action_resolve() -> CodeActionResolveFeature {
    CodeActionResolveFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for CodeActionResolveFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_code_action_resolve();
        Ok(())
    }
}
impl FeatureSpec for CodeActionResolveFeature {
    type Marker = CodeActionResolveRequest;
}

/// The `textDocument/codeLens` feature descriptor. Construct it with
/// [`code_lens`].
pub struct CodeLensFeature {
    options: CodeLensOptions,
}

/// Describe the standard code-lens feature: it dispatches the lsp-types
/// [`CodeLensRequest`] marker and advertises the supplied [`CodeLensOptions`]
/// as the base of the `codeLensProvider` capability family.
pub fn code_lens(options: CodeLensOptions) -> CodeLensFeature {
    CodeLensFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for CodeLensFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_code_lens(self.options)
    }
}
impl FeatureSpec for CodeLensFeature {
    type Marker = CodeLensRequest;
}

/// The `codeLens/resolve` feature descriptor. Construct it with
/// [`code_lens_resolve`].
pub struct CodeLensResolveFeature(());

/// Describe the standard code-lens resolve feature: it dispatches the
/// lsp-types [`CodeLensResolve`] marker — a typed
/// [`CodeLens`](lsp_types::CodeLens) in and out — and augments the code-lens
/// family's capability with `resolveProvider`.
///
/// Resolve is a dependent feature: registering it without the base
/// [`code_lens`] feature fails validation with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability)
/// rather than advertising a dangling `resolveProvider`.
pub fn code_lens_resolve() -> CodeLensResolveFeature {
    CodeLensResolveFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for CodeLensResolveFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_code_lens_resolve();
        Ok(())
    }
}
impl FeatureSpec for CodeLensResolveFeature {
    type Marker = CodeLensResolve;
}

/// The `textDocument/documentLink` feature descriptor. Construct it with
/// [`document_link`].
pub struct DocumentLinkFeature {
    options: DocumentLinkOptions,
}

/// Describe the standard document-link feature: it dispatches the lsp-types
/// [`DocumentLinkRequest`] marker and advertises the supplied
/// [`DocumentLinkOptions`] as the base of the `documentLinkProvider`
/// capability family.
pub fn document_link(options: DocumentLinkOptions) -> DocumentLinkFeature {
    DocumentLinkFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for DocumentLinkFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_document_link(self.options.clone())
    }
}
impl FeatureSpec for DocumentLinkFeature {
    type Marker = DocumentLinkRequest;
}

/// The `documentLink/resolve` feature descriptor. Construct it with
/// [`document_link_resolve`].
pub struct DocumentLinkResolveFeature(());

/// Describe the standard document-link resolve feature: it dispatches the
/// lsp-types [`DocumentLinkResolve`] marker — a typed
/// [`DocumentLink`](lsp_types::DocumentLink) in and out — and augments the
/// document-link family's capability with `resolveProvider`.
///
/// Resolve is a dependent feature: registering it without the base
/// [`document_link`] feature fails validation with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability)
/// rather than advertising a dangling `resolveProvider`.
pub fn document_link_resolve() -> DocumentLinkResolveFeature {
    DocumentLinkResolveFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for DocumentLinkResolveFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_document_link_resolve();
        Ok(())
    }
}
impl FeatureSpec for DocumentLinkResolveFeature {
    type Marker = DocumentLinkResolve;
}

/// The `textDocument/inlayHint` feature descriptor. Construct it with
/// [`inlay_hint`].
pub struct InlayHintFeature {
    options: InlayHintOptions,
}

/// Describe the standard inlay-hint feature: it dispatches the lsp-types
/// [`InlayHintRequest`] marker and advertises the supplied [`InlayHintOptions`]
/// as the base of the `inlayHintProvider` capability family.
pub fn inlay_hint(options: InlayHintOptions) -> InlayHintFeature {
    InlayHintFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for InlayHintFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_inlay_hint(self.options.clone())
    }
}
impl FeatureSpec for InlayHintFeature {
    type Marker = InlayHintRequest;
}

/// The `inlayHint/resolve` feature descriptor. Construct it with
/// [`inlay_hint_resolve`].
pub struct InlayHintResolveFeature(());

/// Describe the standard inlay-hint resolve feature: it dispatches the
/// lsp-types [`InlayHintResolveRequest`] marker — a typed
/// [`InlayHint`](lsp_types::InlayHint) in and out — and augments the
/// inlay-hint family's capability with `resolveProvider`.
///
/// Resolve is a dependent feature: registering it without the base
/// [`inlay_hint`] feature fails validation with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability)
/// rather than advertising a dangling `resolveProvider`.
pub fn inlay_hint_resolve() -> InlayHintResolveFeature {
    InlayHintResolveFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for InlayHintResolveFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_inlay_hint_resolve();
        Ok(())
    }
}
impl FeatureSpec for InlayHintResolveFeature {
    type Marker = InlayHintResolveRequest;
}

/// The `workspace/willCreateFiles` feature descriptor. Construct it with
/// [`will_create_files`].
pub struct WillCreateFilesFeature {
    options: FileOperationRegistrationOptions,
}

/// Describe the standard will-create-files request: it dispatches the
/// lsp-types [`WillCreateFiles`] marker and contributes the supplied
/// [`FileOperationRegistrationOptions`] to the create file-operation family,
/// whose shared filters the family's capability advertises.
///
/// The create family is shared with [`did_create_files`]: both sides must
/// contribute identical filters, or the build fails with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability).
pub fn will_create_files(options: FileOperationRegistrationOptions) -> WillCreateFilesFeature {
    WillCreateFilesFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for WillCreateFilesFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_will_create(self.options.clone())
    }
}
impl FeatureSpec for WillCreateFilesFeature {
    type Marker = WillCreateFiles;
}

/// The `workspace/willRenameFiles` feature descriptor. Construct it with
/// [`will_rename_files`].
pub struct WillRenameFilesFeature {
    options: FileOperationRegistrationOptions,
}

/// Describe the standard will-rename-files request: it dispatches the
/// lsp-types [`WillRenameFiles`] marker and contributes the supplied
/// [`FileOperationRegistrationOptions`] to the rename file-operation family,
/// whose shared filters the family's capability advertises.
///
/// The rename family is shared with [`did_rename_files`]: both sides must
/// contribute identical filters, or the build fails with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability).
pub fn will_rename_files(options: FileOperationRegistrationOptions) -> WillRenameFilesFeature {
    WillRenameFilesFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for WillRenameFilesFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_will_rename(self.options.clone())
    }
}
impl FeatureSpec for WillRenameFilesFeature {
    type Marker = WillRenameFiles;
}

/// The `workspace/willDeleteFiles` feature descriptor. Construct it with
/// [`will_delete_files`].
pub struct WillDeleteFilesFeature {
    options: FileOperationRegistrationOptions,
}

/// Describe the standard will-delete-files request: it dispatches the
/// lsp-types [`WillDeleteFiles`] marker and contributes the supplied
/// [`FileOperationRegistrationOptions`] to the delete file-operation family,
/// whose shared filters the family's capability advertises.
///
/// The delete family is shared with [`did_delete_files`]: both sides must
/// contribute identical filters, or the build fails with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability).
pub fn will_delete_files(options: FileOperationRegistrationOptions) -> WillDeleteFilesFeature {
    WillDeleteFilesFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for WillDeleteFilesFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_will_delete(self.options.clone())
    }
}
impl FeatureSpec for WillDeleteFilesFeature {
    type Marker = WillDeleteFiles;
}

/// The `workspace/didChangeWatchedFiles` notification feature descriptor.
/// Construct it with [`did_change_watched_files`].
pub struct DidChangeWatchedFilesFeature(());

/// Describe the standard watched-files notification feature: it dispatches
/// the lsp-types [`DidChangeWatchedFiles`] marker with typed
/// [`DidChangeWatchedFilesParams`](lsp_types::DidChangeWatchedFilesParams).
///
/// LSP 3.17 has no server capability field for watched files — a server
/// subscribes through dynamic registration — so this descriptor contributes
/// nothing to the capability catalog. Registering through the descriptor
/// rather than [`notification`](crate::ServerBuilder::notification) keeps the
/// registration inside the sealed catalog.
pub fn did_change_watched_files() -> DidChangeWatchedFilesFeature {
    DidChangeWatchedFilesFeature(())
}

#[allow(private_interfaces)]
impl sealed::Sealed for DidChangeWatchedFilesFeature {
    fn contribute(&self, _caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        Ok(())
    }
}
impl NotificationFeatureSpec for DidChangeWatchedFilesFeature {
    type Marker = DidChangeWatchedFiles;
}

/// The `workspace/didCreateFiles` notification feature descriptor. Construct
/// it with [`did_create_files`].
pub struct DidCreateFilesFeature {
    options: FileOperationRegistrationOptions,
}

/// Describe the standard did-create-files notification feature: it dispatches
/// the lsp-types [`DidCreateFiles`] marker and contributes the supplied
/// [`FileOperationRegistrationOptions`] to the create file-operation family,
/// whose shared filters the family's capability advertises.
///
/// The create family is shared with [`will_create_files`]: both sides must
/// contribute identical filters, or the build fails with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability).
pub fn did_create_files(options: FileOperationRegistrationOptions) -> DidCreateFilesFeature {
    DidCreateFilesFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for DidCreateFilesFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_did_create(self.options.clone())
    }
}
impl NotificationFeatureSpec for DidCreateFilesFeature {
    type Marker = DidCreateFiles;
}

/// The `workspace/didRenameFiles` notification feature descriptor. Construct
/// it with [`did_rename_files`].
pub struct DidRenameFilesFeature {
    options: FileOperationRegistrationOptions,
}

/// Describe the standard did-rename-files notification feature: it dispatches
/// the lsp-types [`DidRenameFiles`] marker and contributes the supplied
/// [`FileOperationRegistrationOptions`] to the rename file-operation family,
/// whose shared filters the family's capability advertises.
///
/// The rename family is shared with [`will_rename_files`]: both sides must
/// contribute identical filters, or the build fails with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability).
pub fn did_rename_files(options: FileOperationRegistrationOptions) -> DidRenameFilesFeature {
    DidRenameFilesFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for DidRenameFilesFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_did_rename(self.options.clone())
    }
}
impl NotificationFeatureSpec for DidRenameFilesFeature {
    type Marker = DidRenameFiles;
}

/// The `workspace/didDeleteFiles` notification feature descriptor. Construct
/// it with [`did_delete_files`].
pub struct DidDeleteFilesFeature {
    options: FileOperationRegistrationOptions,
}

/// Describe the standard did-delete-files notification feature: it dispatches
/// the lsp-types [`DidDeleteFiles`] marker and contributes the supplied
/// [`FileOperationRegistrationOptions`] to the delete file-operation family,
/// whose shared filters the family's capability advertises.
///
/// The delete family is shared with [`will_delete_files`]: both sides must
/// contribute identical filters, or the build fails with
/// [`BuildError::ConflictingCapability`](crate::BuildError::ConflictingCapability).
pub fn did_delete_files(options: FileOperationRegistrationOptions) -> DidDeleteFilesFeature {
    DidDeleteFilesFeature { options }
}

#[allow(private_interfaces)]
impl sealed::Sealed for DidDeleteFilesFeature {
    fn contribute(&self, caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
        caps.set_did_delete(self.options.clone())
    }
}
impl NotificationFeatureSpec for DidDeleteFilesFeature {
    type Marker = DidDeleteFiles;
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::notification::{DidChangeWatchedFiles, DidCreateFiles};
    use lsp_types::request::{
        CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
        CodeActionRequest, CodeActionResolveRequest, CodeLensRequest, CodeLensResolve,
        ColorPresentationRequest, DocumentColor, DocumentDiagnosticRequest,
        DocumentHighlightRequest, DocumentLinkRequest, DocumentLinkResolve, DocumentSymbolRequest,
        FoldingRangeRequest, Formatting, GotoDeclaration, GotoDeclarationParams,
        GotoDeclarationResponse, GotoDefinition, GotoImplementation, GotoImplementationParams,
        GotoImplementationResponse, GotoTypeDefinition, GotoTypeDefinitionParams,
        GotoTypeDefinitionResponse, InlayHintRequest, InlayHintResolveRequest, InlineValueRequest,
        LinkedEditingRange, MonikerRequest, OnTypeFormatting, PrepareRenameRequest,
        RangeFormatting, References, Rename, SelectionRangeRequest, SemanticTokensFullDeltaRequest,
        SemanticTokensFullRequest, SemanticTokensRangeRequest, SignatureHelpRequest,
        TypeHierarchyPrepare, TypeHierarchySubtypes, TypeHierarchySupertypes,
        WorkspaceDiagnosticRequest,
    };
    use lsp_types::{
        CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
        CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
        CodeAction, CodeActionOptions, CodeActionParams, CodeActionResponse, CodeLens,
        CodeLensOptions, CodeLensParams, ColorInformation, ColorPresentation,
        ColorPresentationParams, ColorProviderOptions, CreateFilesParams, DeclarationOptions,
        DefinitionOptions, DeleteFilesParams, DiagnosticOptions, DidChangeWatchedFilesParams,
        DocumentColorParams, DocumentDiagnosticParams, DocumentDiagnosticReportResult,
        DocumentFormattingOptions, DocumentFormattingParams, DocumentHighlight,
        DocumentHighlightOptions, DocumentHighlightParams, DocumentLink, DocumentLinkOptions,
        DocumentLinkParams, DocumentOnTypeFormattingOptions, DocumentOnTypeFormattingParams,
        DocumentRangeFormattingOptions, DocumentRangeFormattingParams, DocumentSymbolOptions,
        DocumentSymbolParams, DocumentSymbolResponse, FoldingProviderOptions, FoldingRange,
        FoldingRangeParams, GotoDefinitionParams, GotoDefinitionResponse, InlayHint,
        InlayHintOptions, InlayHintParams, InlineValue, InlineValueOptions, InlineValueParams,
        LinkedEditingRangeOptions, LinkedEditingRangeParams, LinkedEditingRanges, Location,
        Moniker, MonikerOptions, MonikerParams, PrepareRenameResponse, ReferenceParams,
        ReferencesOptions, RenameFilesParams, RenameOptions, RenameParams, SelectionRange,
        SelectionRangeOptions, SelectionRangeParams, SemanticTokensDeltaParams,
        SemanticTokensFullDeltaResult, SemanticTokensParams, SemanticTokensRangeParams,
        SemanticTokensRangeResult, SemanticTokensResult, SignatureHelp, SignatureHelpOptions,
        SignatureHelpParams, StaticTextDocumentRegistrationOptions, TextDocumentPositionParams,
        TextEdit, TypeHierarchyItem, TypeHierarchyPrepareParams, TypeHierarchySubtypesParams,
        TypeHierarchySupertypesParams, WorkspaceDiagnosticParams, WorkspaceDiagnosticReportResult,
        WorkspaceEdit, WorkspaceSymbol, WorkspaceSymbolParams, WorkspaceSymbolResponse,
    };

    fn assert_formatting_descriptor<F: FeatureSpec<Marker = Formatting>>(_: F) {}
    fn assert_range_formatting_descriptor<F: FeatureSpec<Marker = RangeFormatting>>(_: F) {}
    fn assert_on_type_formatting_descriptor<F: FeatureSpec<Marker = OnTypeFormatting>>(_: F) {}

    fn assert_formatting_contract<R>()
    where
        R: Request<Params = DocumentFormattingParams, Result = Option<Vec<TextEdit>>>,
    {
    }

    fn assert_range_formatting_contract<R>()
    where
        R: Request<Params = DocumentRangeFormattingParams, Result = Option<Vec<TextEdit>>>,
    {
    }

    fn assert_on_type_formatting_contract<R>()
    where
        R: Request<Params = DocumentOnTypeFormattingParams, Result = Option<Vec<TextEdit>>>,
    {
    }

    #[test]
    fn formatting_descriptors_fix_the_exact_lsp_types_contracts() {
        assert_formatting_descriptor(document_formatting(DocumentFormattingOptions::default()));
        assert_range_formatting_descriptor(range_formatting(DocumentRangeFormattingOptions {
            work_done_progress_options: Default::default(),
        }));
        assert_on_type_formatting_descriptor(on_type_formatting(
            DocumentOnTypeFormattingOptions::default(),
        ));
        assert_formatting_contract::<Formatting>();
        assert_range_formatting_contract::<RangeFormatting>();
        assert_on_type_formatting_contract::<OnTypeFormatting>();
        assert_eq!(<Formatting as Request>::METHOD, "textDocument/formatting");
        assert_eq!(
            <RangeFormatting as Request>::METHOD,
            "textDocument/rangeFormatting"
        );
        assert_eq!(
            <OnTypeFormatting as Request>::METHOD,
            "textDocument/onTypeFormatting"
        );
    }

    fn assert_document_color_descriptor<F: FeatureSpec<Marker = DocumentColor>>(_: F) {}
    fn assert_color_presentation_descriptor<F: FeatureSpec<Marker = ColorPresentationRequest>>(
        _: F,
    ) {
    }
    fn assert_folding_range_descriptor<F: FeatureSpec<Marker = FoldingRangeRequest>>(_: F) {}
    fn assert_selection_range_descriptor<F: FeatureSpec<Marker = SelectionRangeRequest>>(_: F) {}
    fn assert_inline_value_descriptor<F: FeatureSpec<Marker = InlineValueRequest>>(_: F) {}

    fn assert_document_color_contract<R>()
    where
        R: Request<Params = DocumentColorParams, Result = Vec<ColorInformation>>,
    {
    }

    fn assert_color_presentation_contract<R>()
    where
        R: Request<Params = ColorPresentationParams, Result = Vec<ColorPresentation>>,
    {
    }

    fn assert_folding_range_contract<R>()
    where
        R: Request<Params = FoldingRangeParams, Result = Option<Vec<FoldingRange>>>,
    {
    }

    fn assert_selection_range_contract<R>()
    where
        R: Request<Params = SelectionRangeParams, Result = Option<Vec<SelectionRange>>>,
    {
    }

    fn assert_inline_value_contract<R>()
    where
        R: Request<Params = InlineValueParams, Result = Option<Vec<InlineValue>>>,
    {
    }

    #[test]
    fn presentation_descriptors_fix_the_exact_lsp_types_contracts() {
        assert_document_color_descriptor(document_color(ColorProviderOptions {}));
        assert_color_presentation_descriptor(color_presentation());
        assert_folding_range_descriptor(folding_range(FoldingProviderOptions {}));
        assert_selection_range_descriptor(selection_range(SelectionRangeOptions::default()));
        assert_inline_value_descriptor(inline_value(InlineValueOptions::default()));
        assert_document_color_contract::<DocumentColor>();
        assert_color_presentation_contract::<ColorPresentationRequest>();
        assert_folding_range_contract::<FoldingRangeRequest>();
        assert_selection_range_contract::<SelectionRangeRequest>();
        assert_inline_value_contract::<InlineValueRequest>();
        assert_eq!(
            <DocumentColor as Request>::METHOD,
            "textDocument/documentColor"
        );
        assert_eq!(
            <ColorPresentationRequest as Request>::METHOD,
            "textDocument/colorPresentation"
        );
        assert_eq!(
            <FoldingRangeRequest as Request>::METHOD,
            "textDocument/foldingRange"
        );
        assert_eq!(
            <SelectionRangeRequest as Request>::METHOD,
            "textDocument/selectionRange"
        );
        assert_eq!(
            <InlineValueRequest as Request>::METHOD,
            "textDocument/inlineValue"
        );
    }

    #[test]
    fn editing_and_presentation_descriptors_contribute_only_their_capabilities() {
        let formatting = DocumentFormattingOptions::default();
        let range = DocumentRangeFormattingOptions {
            work_done_progress_options: Default::default(),
        };
        let on_type = DocumentOnTypeFormattingOptions {
            first_trigger_character: "}".to_string(),
            more_trigger_character: Some(vec![";".to_string()]),
        };
        let selection = SelectionRangeOptions::default();
        let inline = InlineValueOptions::default();
        let mut caps = CapabilityBuilder::default();

        sealed::Sealed::contribute(&document_formatting(formatting.clone()), &mut caps).unwrap();
        sealed::Sealed::contribute(&range_formatting(range.clone()), &mut caps).unwrap();
        sealed::Sealed::contribute(&on_type_formatting(on_type.clone()), &mut caps).unwrap();
        sealed::Sealed::contribute(&document_color(ColorProviderOptions {}), &mut caps).unwrap();
        sealed::Sealed::contribute(&color_presentation(), &mut caps).unwrap();
        sealed::Sealed::contribute(&folding_range(FoldingProviderOptions {}), &mut caps).unwrap();
        sealed::Sealed::contribute(&selection_range(selection.clone()), &mut caps).unwrap();
        sealed::Sealed::contribute(&inline_value(inline.clone()), &mut caps).unwrap();
        caps.validate().unwrap();

        assert_eq!(
            caps.finish(),
            lsp_types::ServerCapabilities {
                document_formatting_provider: Some(lsp_types::OneOf::Right(formatting)),
                document_range_formatting_provider: Some(lsp_types::OneOf::Right(range)),
                document_on_type_formatting_provider: Some(on_type),
                color_provider: Some(lsp_types::ColorProviderCapability::ColorProvider(
                    ColorProviderOptions {},
                )),
                folding_range_provider: Some(
                    lsp_types::FoldingRangeProviderCapability::FoldingProvider(
                        FoldingProviderOptions {},
                    ),
                ),
                selection_range_provider: Some(
                    lsp_types::SelectionRangeProviderCapability::Options(selection),
                ),
                inline_value_provider: Some(lsp_types::OneOf::Right(
                    lsp_types::InlineValueServerCapabilities::Options(inline),
                )),
                ..lsp_types::ServerCapabilities::default()
            }
        );
    }

    fn assert_call_prepare_descriptor<F: FeatureSpec<Marker = CallHierarchyPrepare>>(_: F) {}
    fn assert_call_incoming_descriptor<F: FeatureSpec<Marker = CallHierarchyIncomingCalls>>(_: F) {}
    fn assert_call_outgoing_descriptor<F: FeatureSpec<Marker = CallHierarchyOutgoingCalls>>(_: F) {}

    fn assert_call_prepare_contract<R>()
    where
        R: Request<Params = CallHierarchyPrepareParams, Result = Option<Vec<CallHierarchyItem>>>,
    {
    }

    fn assert_call_incoming_contract<R>()
    where
        R: Request<
                Params = CallHierarchyIncomingCallsParams,
                Result = Option<Vec<CallHierarchyIncomingCall>>,
            >,
    {
    }

    fn assert_call_outgoing_contract<R>()
    where
        R: Request<
                Params = CallHierarchyOutgoingCallsParams,
                Result = Option<Vec<CallHierarchyOutgoingCall>>,
            >,
    {
    }

    #[test]
    fn call_hierarchy_descriptors_fix_the_exact_lsp_types_contracts() {
        assert_call_prepare_descriptor(call_hierarchy_prepare(CallHierarchyOptions::default()));
        assert_call_incoming_descriptor(call_hierarchy_incoming_calls());
        assert_call_outgoing_descriptor(call_hierarchy_outgoing_calls());
        assert_call_prepare_contract::<CallHierarchyPrepare>();
        assert_call_incoming_contract::<CallHierarchyIncomingCalls>();
        assert_call_outgoing_contract::<CallHierarchyOutgoingCalls>();
        assert_eq!(
            <CallHierarchyPrepare as Request>::METHOD,
            "textDocument/prepareCallHierarchy"
        );
        assert_eq!(
            <CallHierarchyIncomingCalls as Request>::METHOD,
            "callHierarchy/incomingCalls"
        );
        assert_eq!(
            <CallHierarchyOutgoingCalls as Request>::METHOD,
            "callHierarchy/outgoingCalls"
        );
    }

    fn assert_type_prepare_descriptor<F: FeatureSpec<Marker = TypeHierarchyPrepare>>(_: F) {}
    fn assert_type_supertypes_descriptor<F: FeatureSpec<Marker = TypeHierarchySupertypes>>(_: F) {}
    fn assert_type_subtypes_descriptor<F: FeatureSpec<Marker = TypeHierarchySubtypes>>(_: F) {}

    fn assert_type_prepare_contract<R>()
    where
        R: Request<Params = TypeHierarchyPrepareParams, Result = Option<Vec<TypeHierarchyItem>>>,
    {
    }

    fn assert_type_supertypes_contract<R>()
    where
        R: Request<Params = TypeHierarchySupertypesParams, Result = Option<Vec<TypeHierarchyItem>>>,
    {
    }

    fn assert_type_subtypes_contract<R>()
    where
        R: Request<Params = TypeHierarchySubtypesParams, Result = Option<Vec<TypeHierarchyItem>>>,
    {
    }

    #[test]
    fn type_hierarchy_descriptors_fix_the_exact_lsp_types_contracts() {
        assert_type_prepare_descriptor(type_hierarchy_prepare(TypeHierarchyOptions::default()));
        assert_type_supertypes_descriptor(type_hierarchy_supertypes());
        assert_type_subtypes_descriptor(type_hierarchy_subtypes());
        assert_type_prepare_contract::<TypeHierarchyPrepare>();
        assert_type_supertypes_contract::<TypeHierarchySupertypes>();
        assert_type_subtypes_contract::<TypeHierarchySubtypes>();
        assert_eq!(
            <TypeHierarchyPrepare as Request>::METHOD,
            "textDocument/prepareTypeHierarchy"
        );
        assert_eq!(
            <TypeHierarchySupertypes as Request>::METHOD,
            "typeHierarchy/supertypes"
        );
        assert_eq!(
            <TypeHierarchySubtypes as Request>::METHOD,
            "typeHierarchy/subtypes"
        );
    }

    fn assert_semantic_full_descriptor<F: FeatureSpec<Marker = SemanticTokensFullRequest>>(_: F) {}
    fn assert_semantic_delta_descriptor<F: FeatureSpec<Marker = SemanticTokensFullDeltaRequest>>(
        _: F,
    ) {
    }
    fn assert_semantic_range_descriptor<F: FeatureSpec<Marker = SemanticTokensRangeRequest>>(_: F) {
    }

    fn assert_semantic_full_contract<R>()
    where
        R: Request<Params = SemanticTokensParams, Result = Option<SemanticTokensResult>>,
    {
    }

    fn assert_semantic_delta_contract<R>()
    where
        R: Request<
                Params = SemanticTokensDeltaParams,
                Result = Option<SemanticTokensFullDeltaResult>,
            >,
    {
    }

    fn assert_semantic_range_contract<R>()
    where
        R: Request<Params = SemanticTokensRangeParams, Result = Option<SemanticTokensRangeResult>>,
    {
    }

    #[test]
    fn semantic_token_descriptors_fix_the_exact_lsp_types_contracts() {
        let options = SemanticTokensOptions::default();
        assert_semantic_full_descriptor(semantic_tokens_full(options.clone()));
        assert_semantic_delta_descriptor(semantic_tokens_full_delta(options.clone()));
        assert_semantic_range_descriptor(semantic_tokens_range(options));
        assert_semantic_full_contract::<SemanticTokensFullRequest>();
        assert_semantic_delta_contract::<SemanticTokensFullDeltaRequest>();
        assert_semantic_range_contract::<SemanticTokensRangeRequest>();
        assert_eq!(
            <SemanticTokensFullRequest as Request>::METHOD,
            "textDocument/semanticTokens/full"
        );
        assert_eq!(
            <SemanticTokensFullDeltaRequest as Request>::METHOD,
            "textDocument/semanticTokens/full/delta"
        );
        assert_eq!(
            <SemanticTokensRangeRequest as Request>::METHOD,
            "textDocument/semanticTokens/range"
        );
    }

    fn assert_document_descriptor<F: FeatureSpec<Marker = DocumentDiagnosticRequest>>(_: F) {}

    fn assert_will_save_wait_until_descriptor<F: FeatureSpec<Marker = WillSaveWaitUntil>>(_: F) {}

    #[test]
    fn will_save_wait_until_descriptor_fixes_the_typed_request_contract() {
        assert_will_save_wait_until_descriptor(will_save_wait_until());
        assert_eq!(
            <WillSaveWaitUntil as Request>::METHOD,
            "textDocument/willSaveWaitUntil"
        );
    }
    fn assert_workspace_descriptor<F: FeatureSpec<Marker = WorkspaceDiagnosticRequest>>(_: F) {}

    fn assert_document_contract<R>()
    where
        R: Request<Params = DocumentDiagnosticParams, Result = DocumentDiagnosticReportResult>,
    {
    }

    fn assert_workspace_contract<R>()
    where
        R: Request<Params = WorkspaceDiagnosticParams, Result = WorkspaceDiagnosticReportResult>,
    {
    }

    #[test]
    fn diagnostic_descriptors_fix_the_exact_lsp_types_contracts() {
        assert_document_descriptor(document_diagnostic(DiagnosticOptions::default()));
        assert_workspace_descriptor(workspace_diagnostic(DiagnosticOptions::default()));
        assert_document_contract::<DocumentDiagnosticRequest>();
        assert_workspace_contract::<WorkspaceDiagnosticRequest>();
        assert_eq!(
            <DocumentDiagnosticRequest as Request>::METHOD,
            "textDocument/diagnostic"
        );
        assert_eq!(
            <WorkspaceDiagnosticRequest as Request>::METHOD,
            "workspace/diagnostic"
        );
    }

    fn assert_workspace_symbol_descriptor<F: FeatureSpec<Marker = WorkspaceSymbolRequest>>(_: F) {}
    fn assert_workspace_symbol_resolve_descriptor<
        F: FeatureSpec<Marker = WorkspaceSymbolResolve>,
    >(
        _: F,
    ) {
    }
    fn assert_will_create_descriptor<F: FeatureSpec<Marker = WillCreateFiles>>(_: F) {}
    fn assert_will_rename_descriptor<F: FeatureSpec<Marker = WillRenameFiles>>(_: F) {}
    fn assert_will_delete_descriptor<F: FeatureSpec<Marker = WillDeleteFiles>>(_: F) {}
    fn assert_did_change_watched_descriptor<
        F: NotificationFeatureSpec<Marker = DidChangeWatchedFiles>,
    >(
        _: F,
    ) {
    }
    fn assert_did_create_descriptor<F: NotificationFeatureSpec<Marker = DidCreateFiles>>(_: F) {}

    fn assert_workspace_symbol_contract<R>()
    where
        R: Request<Params = WorkspaceSymbolParams, Result = Option<WorkspaceSymbolResponse>>,
    {
    }

    fn assert_workspace_symbol_resolve_contract<R>()
    where
        R: Request<Params = WorkspaceSymbol, Result = WorkspaceSymbol>,
    {
    }

    fn assert_will_create_contract<R>()
    where
        R: Request<Params = CreateFilesParams, Result = Option<WorkspaceEdit>>,
    {
    }

    fn assert_will_rename_contract<R>()
    where
        R: Request<Params = RenameFilesParams, Result = Option<WorkspaceEdit>>,
    {
    }

    fn assert_will_delete_contract<R>()
    where
        R: Request<Params = DeleteFilesParams, Result = Option<WorkspaceEdit>>,
    {
    }

    fn assert_did_change_watched_contract<N>()
    where
        N: Notification<Params = DidChangeWatchedFilesParams>,
    {
    }

    fn assert_did_create_contract<N>()
    where
        N: Notification<Params = CreateFilesParams>,
    {
    }

    fn workspace_symbol_options() -> WorkspaceSymbolOptions {
        WorkspaceSymbolOptions {
            work_done_progress_options: Default::default(),
            resolve_provider: None,
        }
    }

    #[test]
    fn workspace_symbol_descriptors_fix_the_exact_lsp_types_contracts() {
        assert_workspace_symbol_descriptor(workspace_symbol(workspace_symbol_options()));
        assert_workspace_symbol_resolve_descriptor(workspace_symbol_resolve());
        assert_workspace_symbol_contract::<WorkspaceSymbolRequest>();
        assert_workspace_symbol_resolve_contract::<WorkspaceSymbolResolve>();
        assert_eq!(
            <WorkspaceSymbolRequest as Request>::METHOD,
            "workspace/symbol"
        );
        assert_eq!(
            <WorkspaceSymbolResolve as Request>::METHOD,
            "workspaceSymbol/resolve"
        );
    }

    #[test]
    fn file_operation_descriptors_fix_the_exact_lsp_types_contracts() {
        let options = || FileOperationRegistrationOptions::default();
        assert_will_create_descriptor(will_create_files(options()));
        assert_will_rename_descriptor(will_rename_files(options()));
        assert_will_delete_descriptor(will_delete_files(options()));
        assert_did_change_watched_descriptor(did_change_watched_files());
        assert_did_create_descriptor(did_create_files(options()));
        assert_will_create_contract::<WillCreateFiles>();
        assert_will_rename_contract::<WillRenameFiles>();
        assert_will_delete_contract::<WillDeleteFiles>();
        assert_did_change_watched_contract::<DidChangeWatchedFiles>();
        assert_did_create_contract::<DidCreateFiles>();
        assert_eq!(
            <WillCreateFiles as Request>::METHOD,
            "workspace/willCreateFiles"
        );
        assert_eq!(
            <WillRenameFiles as Request>::METHOD,
            "workspace/willRenameFiles"
        );
        assert_eq!(
            <WillDeleteFiles as Request>::METHOD,
            "workspace/willDeleteFiles"
        );
        assert_eq!(
            <DidChangeWatchedFiles as Notification>::METHOD,
            "workspace/didChangeWatchedFiles"
        );
        assert_eq!(
            <DidCreateFiles as Notification>::METHOD,
            "workspace/didCreateFiles"
        );
        assert_eq!(
            <DidRenameFiles as Notification>::METHOD,
            "workspace/didRenameFiles"
        );
        assert_eq!(
            <DidDeleteFiles as Notification>::METHOD,
            "workspace/didDeleteFiles"
        );
    }

    fn assert_rename_descriptor<F: FeatureSpec<Marker = Rename>>(_: F) {}
    fn assert_prepare_rename_descriptor<F: FeatureSpec<Marker = PrepareRenameRequest>>(_: F) {}

    fn assert_rename_contract<R>()
    where
        R: Request<Params = RenameParams, Result = Option<WorkspaceEdit>>,
    {
    }

    fn assert_prepare_rename_contract<R>()
    where
        R: Request<Params = TextDocumentPositionParams, Result = Option<PrepareRenameResponse>>,
    {
    }

    #[test]
    fn rename_descriptors_fix_the_exact_lsp_types_contracts() {
        assert_rename_descriptor(rename(RenameOptions {
            prepare_provider: None,
            work_done_progress_options: Default::default(),
        }));
        assert_prepare_rename_descriptor(prepare_rename());
        assert_rename_contract::<Rename>();
        assert_prepare_rename_contract::<PrepareRenameRequest>();
        assert_eq!(<Rename as Request>::METHOD, "textDocument/rename");
        assert_eq!(
            <PrepareRenameRequest as Request>::METHOD,
            "textDocument/prepareRename"
        );
    }

    fn assert_code_action_descriptor<F: FeatureSpec<Marker = CodeActionRequest>>(_: F) {}
    fn assert_code_action_resolve_descriptor<F: FeatureSpec<Marker = CodeActionResolveRequest>>(
        _: F,
    ) {
    }

    fn assert_code_action_contract<R>()
    where
        R: Request<Params = CodeActionParams, Result = Option<CodeActionResponse>>,
    {
    }

    fn assert_code_action_resolve_contract<R>()
    where
        R: Request<Params = CodeAction, Result = CodeAction>,
    {
    }

    #[test]
    fn code_action_descriptors_fix_the_exact_lsp_types_contracts() {
        assert_code_action_descriptor(code_action(CodeActionOptions::default()));
        assert_code_action_resolve_descriptor(code_action_resolve());
        assert_code_action_contract::<CodeActionRequest>();
        assert_code_action_resolve_contract::<CodeActionResolveRequest>();
        assert_eq!(
            <CodeActionRequest as Request>::METHOD,
            "textDocument/codeAction"
        );
        assert_eq!(
            <CodeActionResolveRequest as Request>::METHOD,
            "codeAction/resolve"
        );
    }

    fn assert_code_lens_descriptor<F: FeatureSpec<Marker = CodeLensRequest>>(_: F) {}
    fn assert_code_lens_resolve_descriptor<F: FeatureSpec<Marker = CodeLensResolve>>(_: F) {}

    fn assert_code_lens_contract<R>()
    where
        R: Request<Params = CodeLensParams, Result = Option<Vec<CodeLens>>>,
    {
    }

    fn assert_code_lens_resolve_contract<R>()
    where
        R: Request<Params = CodeLens, Result = CodeLens>,
    {
    }

    #[test]
    fn code_lens_descriptors_fix_the_exact_lsp_types_contracts() {
        assert_code_lens_descriptor(code_lens(CodeLensOptions {
            resolve_provider: None,
        }));
        assert_code_lens_resolve_descriptor(code_lens_resolve());
        assert_code_lens_contract::<CodeLensRequest>();
        assert_code_lens_resolve_contract::<CodeLensResolve>();
        assert_eq!(
            <CodeLensRequest as Request>::METHOD,
            "textDocument/codeLens"
        );
        assert_eq!(<CodeLensResolve as Request>::METHOD, "codeLens/resolve");
    }

    fn assert_document_link_descriptor<F: FeatureSpec<Marker = DocumentLinkRequest>>(_: F) {}
    fn assert_document_link_resolve_descriptor<F: FeatureSpec<Marker = DocumentLinkResolve>>(_: F) {
    }

    fn assert_document_link_contract<R>()
    where
        R: Request<Params = DocumentLinkParams, Result = Option<Vec<DocumentLink>>>,
    {
    }

    fn assert_document_link_resolve_contract<R>()
    where
        R: Request<Params = DocumentLink, Result = DocumentLink>,
    {
    }

    #[test]
    fn document_link_descriptors_fix_the_exact_lsp_types_contracts() {
        assert_document_link_descriptor(document_link(DocumentLinkOptions {
            resolve_provider: None,
            work_done_progress_options: Default::default(),
        }));
        assert_document_link_resolve_descriptor(document_link_resolve());
        assert_document_link_contract::<DocumentLinkRequest>();
        assert_document_link_resolve_contract::<DocumentLinkResolve>();
        assert_eq!(
            <DocumentLinkRequest as Request>::METHOD,
            "textDocument/documentLink"
        );
        assert_eq!(
            <DocumentLinkResolve as Request>::METHOD,
            "documentLink/resolve"
        );
    }

    fn assert_inlay_hint_descriptor<F: FeatureSpec<Marker = InlayHintRequest>>(_: F) {}
    fn assert_inlay_hint_resolve_descriptor<F: FeatureSpec<Marker = InlayHintResolveRequest>>(
        _: F,
    ) {
    }

    fn assert_inlay_hint_contract<R>()
    where
        R: Request<Params = InlayHintParams, Result = Option<Vec<InlayHint>>>,
    {
    }

    fn assert_inlay_hint_resolve_contract<R>()
    where
        R: Request<Params = InlayHint, Result = InlayHint>,
    {
    }

    #[test]
    fn inlay_hint_descriptors_fix_the_exact_lsp_types_contracts() {
        assert_inlay_hint_descriptor(inlay_hint(InlayHintOptions::default()));
        assert_inlay_hint_resolve_descriptor(inlay_hint_resolve());
        assert_inlay_hint_contract::<InlayHintRequest>();
        assert_inlay_hint_resolve_contract::<InlayHintResolveRequest>();
        assert_eq!(
            <InlayHintRequest as Request>::METHOD,
            "textDocument/inlayHint"
        );
        assert_eq!(
            <InlayHintResolveRequest as Request>::METHOD,
            "inlayHint/resolve"
        );
    }

    fn assert_signature_help_descriptor<F: FeatureSpec<Marker = SignatureHelpRequest>>(_: F) {}
    fn assert_declaration_descriptor<F: FeatureSpec<Marker = GotoDeclaration>>(_: F) {}
    fn assert_definition_descriptor<F: FeatureSpec<Marker = GotoDefinition>>(_: F) {}
    fn assert_type_definition_descriptor<F: FeatureSpec<Marker = GotoTypeDefinition>>(_: F) {}
    fn assert_implementation_descriptor<F: FeatureSpec<Marker = GotoImplementation>>(_: F) {}
    fn assert_references_descriptor<F: FeatureSpec<Marker = References>>(_: F) {}
    fn assert_document_highlight_descriptor<F: FeatureSpec<Marker = DocumentHighlightRequest>>(
        _: F,
    ) {
    }
    fn assert_document_symbol_descriptor<F: FeatureSpec<Marker = DocumentSymbolRequest>>(_: F) {}
    fn assert_linked_editing_range_descriptor<F: FeatureSpec<Marker = LinkedEditingRange>>(_: F) {}
    fn assert_moniker_descriptor<F: FeatureSpec<Marker = MonikerRequest>>(_: F) {}

    fn assert_signature_help_contract<R>()
    where
        R: Request<Params = SignatureHelpParams, Result = Option<SignatureHelp>>,
    {
    }

    fn assert_declaration_contract<R>()
    where
        R: Request<Params = GotoDeclarationParams, Result = Option<GotoDeclarationResponse>>,
    {
    }

    fn assert_definition_contract<R>()
    where
        R: Request<Params = GotoDefinitionParams, Result = Option<GotoDefinitionResponse>>,
    {
    }

    fn assert_type_definition_contract<R>()
    where
        R: Request<Params = GotoTypeDefinitionParams, Result = Option<GotoTypeDefinitionResponse>>,
    {
    }

    fn assert_implementation_contract<R>()
    where
        R: Request<Params = GotoImplementationParams, Result = Option<GotoImplementationResponse>>,
    {
    }

    fn assert_references_contract<R>()
    where
        R: Request<Params = ReferenceParams, Result = Option<Vec<Location>>>,
    {
    }

    fn assert_document_highlight_contract<R>()
    where
        R: Request<Params = DocumentHighlightParams, Result = Option<Vec<DocumentHighlight>>>,
    {
    }

    fn assert_document_symbol_contract<R>()
    where
        R: Request<Params = DocumentSymbolParams, Result = Option<DocumentSymbolResponse>>,
    {
    }

    fn assert_linked_editing_range_contract<R>()
    where
        R: Request<Params = LinkedEditingRangeParams, Result = Option<LinkedEditingRanges>>,
    {
    }

    fn assert_moniker_contract<R>()
    where
        R: Request<Params = MonikerParams, Result = Option<Vec<Moniker>>>,
    {
    }

    fn progress(value: Option<bool>) -> lsp_types::WorkDoneProgressOptions {
        lsp_types::WorkDoneProgressOptions {
            work_done_progress: value,
        }
    }

    fn static_text_document_registration_options() -> StaticTextDocumentRegistrationOptions {
        StaticTextDocumentRegistrationOptions {
            document_selector: None,
            id: Some("nav".to_string()),
        }
    }

    #[test]
    fn navigation_and_lookup_descriptors_fix_the_exact_lsp_types_contracts() {
        assert_signature_help_descriptor(signature_help(SignatureHelpOptions::default()));
        assert_declaration_descriptor(declaration(DeclarationOptions {
            work_done_progress_options: progress(Some(true)),
        }));
        assert_definition_descriptor(definition(DefinitionOptions {
            work_done_progress_options: progress(Some(true)),
        }));
        assert_type_definition_descriptor(type_definition(
            static_text_document_registration_options(),
        ));
        assert_implementation_descriptor(implementation(
            static_text_document_registration_options(),
        ));
        assert_references_descriptor(references(ReferencesOptions {
            work_done_progress_options: progress(Some(true)),
        }));
        assert_document_highlight_descriptor(document_highlight(DocumentHighlightOptions {
            work_done_progress_options: progress(Some(true)),
        }));
        assert_document_symbol_descriptor(document_symbol(DocumentSymbolOptions {
            label: Some("outline".to_string()),
            work_done_progress_options: progress(Some(true)),
        }));
        assert_linked_editing_range_descriptor(linked_editing_range(LinkedEditingRangeOptions {
            work_done_progress_options: progress(Some(true)),
        }));
        assert_moniker_descriptor(moniker(MonikerOptions {
            work_done_progress_options: progress(Some(true)),
        }));
        assert_signature_help_contract::<SignatureHelpRequest>();
        assert_declaration_contract::<GotoDeclaration>();
        assert_definition_contract::<GotoDefinition>();
        assert_type_definition_contract::<GotoTypeDefinition>();
        assert_implementation_contract::<GotoImplementation>();
        assert_references_contract::<References>();
        assert_document_highlight_contract::<DocumentHighlightRequest>();
        assert_document_symbol_contract::<DocumentSymbolRequest>();
        assert_linked_editing_range_contract::<LinkedEditingRange>();
        assert_moniker_contract::<MonikerRequest>();
        assert_eq!(
            <SignatureHelpRequest as Request>::METHOD,
            "textDocument/signatureHelp"
        );
        assert_eq!(
            <GotoDeclaration as Request>::METHOD,
            "textDocument/declaration"
        );
        assert_eq!(
            <GotoDefinition as Request>::METHOD,
            "textDocument/definition"
        );
        assert_eq!(
            <GotoTypeDefinition as Request>::METHOD,
            "textDocument/typeDefinition"
        );
        assert_eq!(
            <GotoImplementation as Request>::METHOD,
            "textDocument/implementation"
        );
        assert_eq!(<References as Request>::METHOD, "textDocument/references");
        assert_eq!(
            <DocumentHighlightRequest as Request>::METHOD,
            "textDocument/documentHighlight"
        );
        assert_eq!(
            <DocumentSymbolRequest as Request>::METHOD,
            "textDocument/documentSymbol"
        );
        assert_eq!(
            <LinkedEditingRange as Request>::METHOD,
            "textDocument/linkedEditingRange"
        );
        assert_eq!(<MonikerRequest as Request>::METHOD, "textDocument/moniker");
    }

    #[test]
    fn navigation_and_lookup_descriptors_contribute_only_their_capabilities() {
        let signature_options = SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string()]),
            retrigger_characters: None,
            work_done_progress_options: Default::default(),
        };
        let declaration_options = DeclarationOptions {
            work_done_progress_options: progress(Some(true)),
        };
        let definition_options = DefinitionOptions {
            work_done_progress_options: progress(Some(true)),
        };
        let type_definition_options = static_text_document_registration_options();
        let implementation_options = static_text_document_registration_options();
        let references_options = ReferencesOptions {
            work_done_progress_options: progress(Some(true)),
        };
        let highlight_options = DocumentHighlightOptions {
            work_done_progress_options: progress(Some(true)),
        };
        let symbols_options = DocumentSymbolOptions {
            label: Some("outline".to_string()),
            work_done_progress_options: progress(Some(true)),
        };
        let linked_options = LinkedEditingRangeOptions {
            work_done_progress_options: progress(Some(true)),
        };
        let moniker_options = MonikerOptions {
            work_done_progress_options: progress(Some(true)),
        };
        let mut caps = CapabilityBuilder::default();

        sealed::Sealed::contribute(&signature_help(signature_options.clone()), &mut caps).unwrap();
        sealed::Sealed::contribute(&declaration(declaration_options.clone()), &mut caps).unwrap();
        sealed::Sealed::contribute(&definition(definition_options.clone()), &mut caps).unwrap();
        sealed::Sealed::contribute(&type_definition(type_definition_options.clone()), &mut caps)
            .unwrap();
        sealed::Sealed::contribute(&implementation(implementation_options.clone()), &mut caps)
            .unwrap();
        sealed::Sealed::contribute(&references(references_options.clone()), &mut caps).unwrap();
        sealed::Sealed::contribute(&document_highlight(highlight_options.clone()), &mut caps)
            .unwrap();
        sealed::Sealed::contribute(&document_symbol(symbols_options.clone()), &mut caps).unwrap();
        sealed::Sealed::contribute(&linked_editing_range(linked_options.clone()), &mut caps)
            .unwrap();
        sealed::Sealed::contribute(&moniker(moniker_options.clone()), &mut caps).unwrap();
        caps.validate().unwrap();

        assert_eq!(
            caps.finish(),
            lsp_types::ServerCapabilities {
                signature_help_provider: Some(signature_options),
                declaration_provider: Some(lsp_types::DeclarationCapability::Options(
                    declaration_options
                )),
                definition_provider: Some(lsp_types::OneOf::Right(definition_options)),
                type_definition_provider: Some(
                    lsp_types::TypeDefinitionProviderCapability::Options(type_definition_options)
                ),
                implementation_provider: Some(
                    lsp_types::ImplementationProviderCapability::Options(implementation_options)
                ),
                references_provider: Some(lsp_types::OneOf::Right(references_options)),
                document_highlight_provider: Some(lsp_types::OneOf::Right(highlight_options)),
                document_symbol_provider: Some(lsp_types::OneOf::Right(symbols_options)),
                linked_editing_range_provider: Some(
                    lsp_types::LinkedEditingRangeServerCapabilities::Options(linked_options),
                ),
                moniker_provider: Some(lsp_types::OneOf::Right(
                    lsp_types::MonikerServerCapabilities::Options(moniker_options),
                )),
                ..lsp_types::ServerCapabilities::default()
            }
        );
    }

    #[test]
    fn no_request_descriptor_fixes_the_execute_command_method() {
        // `workspace/executeCommand` is reserved for the Command registry
        // (ADR 0022). The enforcement is structural — `FeatureSpec` is sealed,
        // so no descriptor for it can ever be added downstream — and this pins
        // the in-crate catalog to that rule.
        for method in [
            <HoverRequest as Request>::METHOD,
            <Completion as Request>::METHOD,
            <ResolveCompletionItem as Request>::METHOD,
            <DocumentDiagnosticRequest as Request>::METHOD,
            <WorkspaceDiagnosticRequest as Request>::METHOD,
            <WorkspaceSymbolRequest as Request>::METHOD,
            <WorkspaceSymbolResolve as Request>::METHOD,
            <Rename as Request>::METHOD,
            <PrepareRenameRequest as Request>::METHOD,
            <CodeActionRequest as Request>::METHOD,
            <CodeActionResolveRequest as Request>::METHOD,
            <CodeLensRequest as Request>::METHOD,
            <CodeLensResolve as Request>::METHOD,
            <DocumentLinkRequest as Request>::METHOD,
            <DocumentLinkResolve as Request>::METHOD,
            <InlayHintRequest as Request>::METHOD,
            <InlayHintResolveRequest as Request>::METHOD,
            <WillCreateFiles as Request>::METHOD,
            <WillRenameFiles as Request>::METHOD,
            <WillDeleteFiles as Request>::METHOD,
        ] {
            assert_ne!(method, "workspace/executeCommand");
        }
    }
}
