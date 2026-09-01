//! The internal capability catalog (ADR 0017).
//!
//! Standard features and commands each contribute to one destination field of
//! [`ServerCapabilities`]. [`CapabilityBuilder`] accumulates those
//! contributions by field rather than by method, so a family spread across
//! several methods — completion and completion-item resolve merging into one
//! `completionProvider`, or execute-command merging every command into one
//! de-duplicated list — produces a single deterministic capability. Family
//! merging is independent of registration order; the execute-command list
//! preserves registration order (ADR 0022). Custom requests and notifications
//! contribute nothing.
//!
//! Merging never uses last-write-wins: a contribution that disagrees with an
//! already-recorded singular field, and a dependent contribution whose base
//! is absent, are both a [`BuildError::ConflictingCapability`] surfaced by
//! [`ServerBuilder::build`](crate::ServerBuilder::build) or by the initialize
//! transaction's commit.

use gen_lsp_types::{
    CallHierarchyOptions, CodeActionOptions, CodeLensOptions, CompletionOptions,
    DeclarationOptions, DefinitionOptions, DiagnosticOptions, DocumentColorOptions,
    DocumentFormattingOptions, DocumentHighlightOptions, DocumentLinkOptions,
    DocumentOnTypeFormattingOptions, DocumentRangeFormattingOptions, DocumentSymbolOptions,
    ExecuteCommandOptions, FileOperationOptions, FileOperationRegistrationOptions,
    FoldingRangeOptions, Full, ImplementationRegistrationOptions, InlayHintOptions,
    InlineValueOptions, LinkedEditingRangeOptions, MonikerOptions, ReferenceOptions, RenameOptions,
    SelectionRangeOptions, SemanticTokensFullDelta, SemanticTokensOptions,
    SemanticTokensOptionsRange, ServerCapabilities, SignatureHelpOptions,
    TypeDefinitionRegistrationOptions, TypeHierarchyOptions, WorkspaceOptions,
    WorkspaceSymbolOptions,
};

use crate::error::BuildError;

/// Accumulates standard-feature and command capability contributions and
/// freezes them into one [`ServerCapabilities`] value.
///
/// Multi-method families accumulate beside the in-progress
/// `ServerCapabilities` rather than in it: the completion family keeps its
/// base options and its resolve flag together so the base feature and the
/// completion-item resolve feature emit one merged `completionProvider`
/// regardless of registration order. The ordered `Vec` of command names makes
/// the execute-command list de-duplicated and registration-order preserving
/// (ADR 0022).
/// Protocol-owned negotiated fields (for example ADR 0016's position
/// encoding) are layered on separately by the engine and never pass through
/// here.
#[derive(Default)]
pub(crate) struct CapabilityBuilder {
    caps: ServerCapabilities,
    commands: Vec<String>,
    call_hierarchy: BaseDependentFamily<CallHierarchyOptions>,
    type_hierarchy: BaseDependentFamily<TypeHierarchyOptions>,
    color: BaseDependentFamily<DocumentColorOptions>,
    semantic_tokens: SemanticTokensFamily,
    completion: CompletionFamily,
    diagnostics: DiagnosticFamily,
    workspace_symbols: WorkspaceSymbolFamily,
    rename: RenameFamily,
    code_actions: CodeActionFamily,
    code_lens: CodeLensFamily,
    document_links: DocumentLinkFamily,
    inlay_hints: InlayHintFamily,
    file_create: FileOperationFamily,
    file_rename: FileOperationFamily,
    file_delete: FileOperationFamily,
    will_save_wait_until: bool,
}

/// A capability family with one option-bearing base route and one or more
/// dependent routes that advertise no separate provider.
///
/// Hierarchy prepare routes and document color are bases; hierarchy traversal
/// and color presentation are their respective dependents. Tracking dependent
/// presence lets validation reject incomplete families deterministically.
struct BaseDependentFamily<Options> {
    options: Option<Options>,
    has_subordinate: bool,
}

impl<Options> Default for BaseDependentFamily<Options> {
    fn default() -> Self {
        Self {
            options: None,
            has_subordinate: false,
        }
    }
}

impl<Options: PartialEq> BaseDependentFamily<Options> {
    fn contribute_base(&mut self, options: Options, field: &'static str) -> Result<(), BuildError> {
        match &self.options {
            Some(existing) if *existing != options => {
                Err(BuildError::ConflictingCapability { field })
            }
            _ => {
                self.options = Some(options);
                Ok(())
            }
        }
    }

    fn contribute_subordinate(&mut self) {
        self.has_subordinate = true;
    }

    fn validate(&self, field: &'static str) -> Result<(), BuildError> {
        if self.options.is_none() && self.has_subordinate {
            return Err(BuildError::ConflictingCapability { field });
        }
        Ok(())
    }
}

fn contribute_singular<T: PartialEq>(
    target: &mut Option<T>,
    contribution: T,
    field: &'static str,
) -> Result<(), BuildError> {
    match target {
        Some(existing) if *existing != contribution => {
            Err(BuildError::ConflictingCapability { field })
        }
        _ => {
            *target = Some(contribution);
            Ok(())
        }
    }
}

/// The complete capability object frozen from the registration catalog.
pub(crate) type GeneratedCapabilities = ServerCapabilities;

#[derive(Clone, Default)]
struct SemanticTokensFamily {
    shared_options: Option<SemanticTokensOptions>,
    full: bool,
    delta: bool,
    range: bool,
    declared_full: Option<bool>,
    declared_delta: Option<bool>,
    declared_range: Option<bool>,
}

#[derive(Clone, Copy)]
enum SemanticTokensMode {
    Full,
    Delta,
    Range,
}

impl SemanticTokensFamily {
    fn contribute(
        &mut self,
        mut options: SemanticTokensOptions,
        mode: SemanticTokensMode,
    ) -> Result<(), BuildError> {
        let (declared_full, declared_delta) = match options.full.take() {
            None => (None, None),
            Some(Full::Bool(value)) => (Some(value), None),
            Some(Full::SemanticTokensFullDelta(options)) => (Some(true), options.delta),
        };
        let declared_range = options.range.take().map(|range| match range {
            SemanticTokensOptionsRange::Bool(value) => value,
            SemanticTokensOptionsRange::Object(_) => true,
        });
        if self
            .shared_options
            .as_ref()
            .is_some_and(|existing| *existing != options)
        {
            return Err(BuildError::ConflictingCapability {
                field: "semanticTokensProvider",
            });
        }

        let mut next = self.clone();
        next.shared_options = Some(options);
        Self::merge_declaration(&mut next.declared_full, declared_full)?;
        Self::merge_declaration(&mut next.declared_delta, declared_delta)?;
        Self::merge_declaration(&mut next.declared_range, declared_range)?;
        match mode {
            SemanticTokensMode::Full => next.full = true,
            SemanticTokensMode::Delta => next.delta = true,
            SemanticTokensMode::Range => next.range = true,
        }
        *self = next;
        Ok(())
    }

    fn merge_declaration(
        target: &mut Option<bool>,
        contribution: Option<bool>,
    ) -> Result<(), BuildError> {
        match (*target, contribution) {
            (Some(existing), Some(value)) if existing != value => {
                Err(BuildError::ConflictingCapability {
                    field: "semanticTokensProvider",
                })
            }
            (None, Some(value)) => {
                *target = Some(value);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn validate(&self) -> Result<(), BuildError> {
        let modes_match = self.declared_full.is_none_or(|value| value == self.full)
            && self.declared_delta.is_none_or(|value| value == self.delta)
            && self.declared_range.is_none_or(|value| value == self.range);
        if !modes_match || (self.delta && !self.full) {
            return Err(BuildError::ConflictingCapability {
                field: "semanticTokensProvider",
            });
        }
        Ok(())
    }

    fn finish(mut self) -> Option<SemanticTokensOptions> {
        let mut options = self.shared_options.take()?;
        options.range = self.range.then_some(true.into());
        options.full = if self.delta {
            Some(Full::SemanticTokensFullDelta(SemanticTokensFullDelta {
                delta: Some(true),
            }))
        } else if self.full {
            Some(Full::Bool(true))
        } else {
            None
        };
        Some(options)
    }
}

/// The in-progress `completionProvider` capability family (ADR 0017). The
/// base completion feature contributes the singular [`CompletionOptions`];
/// the completion-item resolve feature contributes only its presence, which
/// [`finish`](CapabilityBuilder::finish) folds into `resolveProvider` on the
/// same capability.
#[derive(Default)]
struct CompletionFamily {
    options: Option<CompletionOptions>,
    resolve: bool,
}

/// The in-progress `diagnosticProvider` capability family. Document and
/// workspace routes share one options value, so either route can contribute
/// the provider without allowing its singular options to drift.
#[derive(Default)]
struct DiagnosticFamily {
    options: Option<DiagnosticOptions>,
}

/// The in-progress `workspaceSymbolProvider` capability family. The base
/// workspace-symbol feature contributes the singular [`WorkspaceSymbolOptions`];
/// the workspace-symbol resolve feature contributes only its presence, which
/// [`finish`](CapabilityBuilder::finish) folds into `resolveProvider` on the
/// same capability.
#[derive(Default)]
struct WorkspaceSymbolFamily {
    options: Option<WorkspaceSymbolOptions>,
    resolve: bool,
}

/// The in-progress `renameProvider` capability family. The base rename
/// feature contributes the singular [`RenameOptions`]; the prepare-rename
/// feature contributes only its presence, which
/// [`finish`](CapabilityBuilder::finish) folds into `prepareProvider` on the
/// same capability.
#[derive(Default)]
struct RenameFamily {
    options: Option<RenameOptions>,
    prepare: bool,
}

/// The in-progress `codeActionProvider` capability family. The base
/// code-action feature contributes the singular [`CodeActionOptions`]; the
/// code-action resolve feature contributes only its presence, which
/// [`finish`](CapabilityBuilder::finish) folds into `resolveProvider` on the
/// same capability.
#[derive(Default)]
struct CodeActionFamily {
    options: Option<CodeActionOptions>,
    resolve: bool,
}

/// The in-progress `codeLensProvider` capability family. The base code-lens
/// feature contributes the singular [`CodeLensOptions`]; the code-lens resolve
/// feature contributes only its presence, which
/// [`finish`](CapabilityBuilder::finish) folds into `resolveProvider` on the
/// same capability.
#[derive(Default)]
struct CodeLensFamily {
    options: Option<CodeLensOptions>,
    resolve: bool,
}

/// The in-progress `documentLinkProvider` capability family. The base
/// document-link feature contributes the singular [`DocumentLinkOptions`]; the
/// document-link resolve feature contributes only its presence, which
/// [`finish`](CapabilityBuilder::finish) folds into `resolveProvider` on the
/// same capability.
#[derive(Default)]
struct DocumentLinkFamily {
    options: Option<DocumentLinkOptions>,
    resolve: bool,
}

/// The in-progress `inlayHintProvider` capability family. The base inlay-hint
/// feature contributes the singular [`InlayHintOptions`]; the inlay-hint
/// resolve feature contributes only its presence, which
/// [`finish`](CapabilityBuilder::finish) folds into `resolveProvider` on the
/// same capability.
#[derive(Default)]
struct InlayHintFamily {
    options: Option<InlayHintOptions>,
    resolve: bool,
}

/// The in-progress state of one file-operation family (create, rename, or
/// delete). The family's `will*` request route and its `did*` notification
/// route share one [`FileOperationRegistrationOptions`] value, so either side
/// can contribute the family without allowing its singular filters to drift;
/// `will` and `did` record which sides registered, and
/// [`finish`](CapabilityBuilder::finish) advertises exactly those sides.
#[derive(Default)]
struct FileOperationFamily {
    options: Option<FileOperationRegistrationOptions>,
    will: bool,
    did: bool,
}

impl FileOperationFamily {
    /// Contribute registration options to this family, marking the
    /// contributing side. Identical options merge; a disagreement is a
    /// [`BuildError::ConflictingCapability`] naming the family rather than a
    /// silent overwrite.
    fn contribute(
        &mut self,
        options: FileOperationRegistrationOptions,
        will: bool,
        field: &'static str,
    ) -> Result<(), BuildError> {
        match &self.options {
            Some(existing) if *existing != options => {
                Err(BuildError::ConflictingCapability { field })
            }
            _ => {
                self.options = Some(options);
                if will {
                    self.will = true;
                } else {
                    self.did = true;
                }
                Ok(())
            }
        }
    }

    /// The family's shared options when its `will*` side registered.
    fn will_options(&self) -> Option<FileOperationRegistrationOptions> {
        self.will.then(|| self.options.clone()).flatten()
    }

    /// The family's shared options when its `did*` side registered.
    fn did_options(&self) -> Option<FileOperationRegistrationOptions> {
        self.did.then(|| self.options.clone()).flatten()
    }
}

impl CapabilityBuilder {
    /// Record the typed `textDocument/willSaveWaitUntil` contribution. The
    /// protocol engine folds this into its protocol-owned sync options.
    pub(crate) fn set_will_save_wait_until(&mut self) {
        self.will_save_wait_until = true;
    }

    pub(crate) fn has_will_save_wait_until(&self) -> bool {
        self.will_save_wait_until
    }

    pub(crate) fn set_call_hierarchy(
        &mut self,
        options: CallHierarchyOptions,
    ) -> Result<(), BuildError> {
        self.call_hierarchy
            .contribute_base(options, "callHierarchyProvider")
    }

    pub(crate) fn set_call_hierarchy_incoming_calls(&mut self) {
        self.call_hierarchy.contribute_subordinate();
    }

    pub(crate) fn set_call_hierarchy_outgoing_calls(&mut self) {
        self.call_hierarchy.contribute_subordinate();
    }

    pub(crate) fn set_type_hierarchy(
        &mut self,
        options: TypeHierarchyOptions,
    ) -> Result<(), BuildError> {
        self.type_hierarchy
            .contribute_base(options, "typeHierarchyProvider")
    }

    pub(crate) fn set_type_hierarchy_supertypes(&mut self) {
        self.type_hierarchy.contribute_subordinate();
    }

    pub(crate) fn set_type_hierarchy_subtypes(&mut self) {
        self.type_hierarchy.contribute_subordinate();
    }

    pub(crate) fn set_semantic_tokens_full(
        &mut self,
        options: SemanticTokensOptions,
    ) -> Result<(), BuildError> {
        self.semantic_tokens
            .contribute(options, SemanticTokensMode::Full)
    }

    pub(crate) fn set_semantic_tokens_full_delta(
        &mut self,
        options: SemanticTokensOptions,
    ) -> Result<(), BuildError> {
        self.semantic_tokens
            .contribute(options, SemanticTokensMode::Delta)
    }

    pub(crate) fn set_semantic_tokens_range(
        &mut self,
        options: SemanticTokensOptions,
    ) -> Result<(), BuildError> {
        self.semantic_tokens
            .contribute(options, SemanticTokensMode::Range)
    }

    pub(crate) fn set_document_formatting(
        &mut self,
        options: DocumentFormattingOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.document_formatting_provider,
            options.into(),
            "documentFormattingProvider",
        )
    }

    pub(crate) fn set_range_formatting(
        &mut self,
        options: DocumentRangeFormattingOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.document_range_formatting_provider,
            options.into(),
            "documentRangeFormattingProvider",
        )
    }

    pub(crate) fn set_on_type_formatting(
        &mut self,
        options: DocumentOnTypeFormattingOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.document_on_type_formatting_provider,
            options,
            "documentOnTypeFormattingProvider",
        )
    }

    pub(crate) fn set_document_color(
        &mut self,
        options: DocumentColorOptions,
    ) -> Result<(), BuildError> {
        self.color.contribute_base(options, "colorProvider")
    }

    pub(crate) fn set_color_presentation(&mut self) {
        self.color.contribute_subordinate();
    }

    pub(crate) fn set_folding_range(
        &mut self,
        options: FoldingRangeOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.folding_range_provider,
            options.into(),
            "foldingRangeProvider",
        )
    }

    pub(crate) fn set_selection_range(
        &mut self,
        options: SelectionRangeOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.selection_range_provider,
            options.into(),
            "selectionRangeProvider",
        )
    }

    pub(crate) fn set_inline_value(
        &mut self,
        options: InlineValueOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.inline_value_provider,
            options.into(),
            "inlineValueProvider",
        )
    }

    /// Contribute the hover capability. Hover carries no options, so repeated
    /// contributions are identical and never conflict; the caller already
    /// rejects a duplicate `textDocument/hover` handler before reaching here.
    pub(crate) fn set_hover(&mut self) -> Result<(), BuildError> {
        self.caps.hover_provider = Some(true.into());
        Ok(())
    }

    /// Contribute the supplied signature-help options as the singular
    /// `signatureHelpProvider`. Two signature-help features that advertise
    /// different options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_signature_help(
        &mut self,
        options: SignatureHelpOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.signature_help_provider,
            options,
            "signatureHelpProvider",
        )
    }

    /// Contribute the supplied declaration options as the singular
    /// `declarationProvider`. Two declaration features that advertise
    /// different options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_declaration(
        &mut self,
        options: DeclarationOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.declaration_provider,
            options.into(),
            "declarationProvider",
        )
    }

    /// Contribute the supplied definition options as the singular
    /// `definitionProvider`. Two definition features that advertise different
    /// options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_definition(&mut self, options: DefinitionOptions) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.definition_provider,
            options.into(),
            "definitionProvider",
        )
    }

    /// Contribute the supplied registration options as the singular
    /// `typeDefinitionProvider`. Two type-definition features that advertise
    /// different options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_type_definition(
        &mut self,
        options: TypeDefinitionRegistrationOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.type_definition_provider,
            options.into(),
            "typeDefinitionProvider",
        )
    }

    /// Contribute the supplied registration options as the singular
    /// `implementationProvider`. Two implementation features that advertise
    /// different options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_implementation(
        &mut self,
        options: ImplementationRegistrationOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.implementation_provider,
            options.into(),
            "implementationProvider",
        )
    }

    /// Contribute the supplied references options as the singular
    /// `referencesProvider`. Two references features that advertise different
    /// options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_references(&mut self, options: ReferenceOptions) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.references_provider,
            options.into(),
            "referencesProvider",
        )
    }

    /// Contribute the supplied document-highlight options as the singular
    /// `documentHighlightProvider`. Two document-highlight features that
    /// advertise different options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_document_highlight(
        &mut self,
        options: DocumentHighlightOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.document_highlight_provider,
            options.into(),
            "documentHighlightProvider",
        )
    }

    /// Contribute the supplied document-symbol options as the singular
    /// `documentSymbolProvider`. Two document-symbol features that advertise
    /// different options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_document_symbol(
        &mut self,
        options: DocumentSymbolOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.document_symbol_provider,
            options.into(),
            "documentSymbolProvider",
        )
    }

    /// Contribute the supplied linked-editing-range options as the singular
    /// `linkedEditingRangeProvider`. Two linked-editing-range features that
    /// advertise different options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_linked_editing_range(
        &mut self,
        options: LinkedEditingRangeOptions,
    ) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.linked_editing_range_provider,
            options.into(),
            "linkedEditingRangeProvider",
        )
    }

    /// Contribute the supplied moniker options as the singular
    /// `monikerProvider`. Two moniker features that advertise different
    /// options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_moniker(&mut self, options: MonikerOptions) -> Result<(), BuildError> {
        contribute_singular(
            &mut self.caps.moniker_provider,
            options.into(),
            "monikerProvider",
        )
    }

    /// Contribute the supplied completion options as the base of the
    /// completion family. Two completion features that advertise different
    /// options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_completion(&mut self, options: CompletionOptions) -> Result<(), BuildError> {
        match &self.completion.options {
            Some(existing) if *existing != options => Err(BuildError::ConflictingCapability {
                field: "completionProvider",
            }),
            _ => {
                self.completion.options = Some(options);
                Ok(())
            }
        }
    }

    /// Contribute completion-item resolve support to the completion family.
    ///
    /// Resolve carries no options of its own, so the contribution is
    /// idempotent. The family owns `resolveProvider`: [`finish`](Self::finish)
    /// sets it on the base options when this was contributed. A resolve
    /// contribution without a base is not rejected here — the check is
    /// deferred to [`validate`](Self::validate) so the merge stays
    /// independent of registration order.
    pub(crate) fn set_completion_resolve(&mut self) {
        self.completion.resolve = true;
    }

    /// Contribute options to the shared document/workspace diagnostics
    /// capability. Every route must agree on the complete provider options;
    /// unequal contributions fail instead of making output order-dependent.
    pub(crate) fn set_diagnostics(&mut self, options: DiagnosticOptions) -> Result<(), BuildError> {
        match &self.diagnostics.options {
            Some(existing) if *existing != options => Err(BuildError::ConflictingCapability {
                field: "diagnosticProvider",
            }),
            _ => {
                self.diagnostics.options = Some(options);
                Ok(())
            }
        }
    }

    /// Contribute the supplied workspace-symbol options as the base of the
    /// workspace-symbol family. Two workspace-symbol features that advertise
    /// different options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_workspace_symbol(
        &mut self,
        options: WorkspaceSymbolOptions,
    ) -> Result<(), BuildError> {
        match &self.workspace_symbols.options {
            Some(existing) if *existing != options => Err(BuildError::ConflictingCapability {
                field: "workspaceSymbolProvider",
            }),
            _ => {
                self.workspace_symbols.options = Some(options);
                Ok(())
            }
        }
    }

    /// Contribute workspace-symbol resolve support to the workspace-symbol
    /// family. Like completion resolve, the contribution is idempotent and a
    /// resolve without its base is deferred to [`validate`](Self::validate).
    pub(crate) fn set_workspace_symbol_resolve(&mut self) {
        self.workspace_symbols.resolve = true;
    }

    /// Contribute the supplied rename options as the base of the rename
    /// family. Two rename features that advertise different options cannot
    /// both be honored, so a mismatch is a [`BuildError::ConflictingCapability`]
    /// rather than a silent overwrite.
    pub(crate) fn set_rename(&mut self, options: RenameOptions) -> Result<(), BuildError> {
        match &self.rename.options {
            Some(existing) if *existing != options => Err(BuildError::ConflictingCapability {
                field: "renameProvider",
            }),
            _ => {
                self.rename.options = Some(options);
                Ok(())
            }
        }
    }

    /// Contribute prepare-rename support to the rename family. Like completion
    /// resolve, the contribution is idempotent and a prepare without its base
    /// is deferred to [`validate`](Self::validate).
    pub(crate) fn set_prepare_rename(&mut self) {
        self.rename.prepare = true;
    }

    /// Contribute the supplied code-action options as the base of the
    /// code-action family. Two code-action features that advertise different
    /// options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_code_action(&mut self, options: CodeActionOptions) -> Result<(), BuildError> {
        match &self.code_actions.options {
            Some(existing) if *existing != options => Err(BuildError::ConflictingCapability {
                field: "codeActionProvider",
            }),
            _ => {
                self.code_actions.options = Some(options);
                Ok(())
            }
        }
    }

    /// Contribute code-action resolve support to the code-action family. Like
    /// completion resolve, the contribution is idempotent and a resolve
    /// without its base is deferred to [`validate`](Self::validate).
    pub(crate) fn set_code_action_resolve(&mut self) {
        self.code_actions.resolve = true;
    }

    /// Contribute the supplied code-lens options as the base of the code-lens
    /// family. Two code-lens features that advertise different options cannot
    /// both be honored, so a mismatch is a [`BuildError::ConflictingCapability`]
    /// rather than a silent overwrite.
    pub(crate) fn set_code_lens(&mut self, options: CodeLensOptions) -> Result<(), BuildError> {
        match &self.code_lens.options {
            Some(existing) if *existing != options => Err(BuildError::ConflictingCapability {
                field: "codeLensProvider",
            }),
            _ => {
                self.code_lens.options = Some(options);
                Ok(())
            }
        }
    }

    /// Contribute code-lens resolve support to the code-lens family. Like
    /// completion resolve, the contribution is idempotent and a resolve
    /// without its base is deferred to [`validate`](Self::validate).
    pub(crate) fn set_code_lens_resolve(&mut self) {
        self.code_lens.resolve = true;
    }

    /// Contribute the supplied document-link options as the base of the
    /// document-link family. Two document-link features that advertise
    /// different options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_document_link(
        &mut self,
        options: DocumentLinkOptions,
    ) -> Result<(), BuildError> {
        match &self.document_links.options {
            Some(existing) if *existing != options => Err(BuildError::ConflictingCapability {
                field: "documentLinkProvider",
            }),
            _ => {
                self.document_links.options = Some(options);
                Ok(())
            }
        }
    }

    /// Contribute document-link resolve support to the document-link family.
    /// Like completion resolve, the contribution is idempotent and a resolve
    /// without its base is deferred to [`validate`](Self::validate).
    pub(crate) fn set_document_link_resolve(&mut self) {
        self.document_links.resolve = true;
    }

    /// Contribute the supplied inlay-hint options as the base of the
    /// inlay-hint family. Two inlay-hint features that advertise different
    /// options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_inlay_hint(&mut self, options: InlayHintOptions) -> Result<(), BuildError> {
        match &self.inlay_hints.options {
            Some(existing) if *existing != options => Err(BuildError::ConflictingCapability {
                field: "inlayHintProvider",
            }),
            _ => {
                self.inlay_hints.options = Some(options);
                Ok(())
            }
        }
    }

    /// Contribute inlay-hint resolve support to the inlay-hint family. Like
    /// completion resolve, the contribution is idempotent and a resolve
    /// without its base is deferred to [`validate`](Self::validate).
    pub(crate) fn set_inlay_hint_resolve(&mut self) {
        self.inlay_hints.resolve = true;
    }

    /// Contribute the supplied filters to the create file-operation family and
    /// advertise its `willCreateFiles` side. The `will*` and `did*` sides of a
    /// family share one filters value; unequal contributions fail instead of
    /// making output order-dependent.
    pub(crate) fn set_will_create(
        &mut self,
        options: FileOperationRegistrationOptions,
    ) -> Result<(), BuildError> {
        self.file_create
            .contribute(options, true, "workspace.fileOperations.create")
    }

    /// Contribute the supplied filters to the create file-operation family and
    /// advertise its `didCreateFiles` side.
    pub(crate) fn set_did_create(
        &mut self,
        options: FileOperationRegistrationOptions,
    ) -> Result<(), BuildError> {
        self.file_create
            .contribute(options, false, "workspace.fileOperations.create")
    }

    /// Contribute the supplied filters to the rename file-operation family and
    /// advertise its `willRenameFiles` side.
    pub(crate) fn set_will_rename(
        &mut self,
        options: FileOperationRegistrationOptions,
    ) -> Result<(), BuildError> {
        self.file_rename
            .contribute(options, true, "workspace.fileOperations.rename")
    }

    /// Contribute the supplied filters to the rename file-operation family and
    /// advertise its `didRenameFiles` side.
    pub(crate) fn set_did_rename(
        &mut self,
        options: FileOperationRegistrationOptions,
    ) -> Result<(), BuildError> {
        self.file_rename
            .contribute(options, false, "workspace.fileOperations.rename")
    }

    /// Contribute the supplied filters to the delete file-operation family and
    /// advertise its `willDeleteFiles` side.
    pub(crate) fn set_will_delete(
        &mut self,
        options: FileOperationRegistrationOptions,
    ) -> Result<(), BuildError> {
        self.file_delete
            .contribute(options, true, "workspace.fileOperations.delete")
    }

    /// Contribute the supplied filters to the delete file-operation family and
    /// advertise its `didDeleteFiles` side.
    pub(crate) fn set_did_delete(
        &mut self,
        options: FileOperationRegistrationOptions,
    ) -> Result<(), BuildError> {
        self.file_delete
            .contribute(options, false, "workspace.fileOperations.delete")
    }
    /// Cross-contribution validation a single contribution cannot perform on
    /// its own: a dependent feature whose required base is absent, or a base
    /// and a dependent that disagree on a singular family field. Run both at
    /// static build time and when the initialize transaction commits, so the
    /// rule applies identically to static and conditional registrations.
    pub(crate) fn validate(&self) -> Result<(), BuildError> {
        self.call_hierarchy.validate("callHierarchyProvider")?;
        self.type_hierarchy.validate("typeHierarchyProvider")?;
        self.color.validate("colorProvider")?;
        self.semantic_tokens.validate()?;

        let clash = || BuildError::ConflictingCapability {
            field: "completionProvider",
        };
        match &self.completion.options {
            // Resolve without its base feature would advertise a dangling
            // `resolveProvider`.
            None if self.completion.resolve => return Err(clash()),
            // The resolve feature contributes `resolveProvider = true`; a base
            // that explicitly denies it cannot be honored by last-write-wins.
            Some(options) if self.completion.resolve && options.resolve_provider == Some(false) => {
                return Err(clash());
            }
            _ => {}
        }

        let symbol_clash = || BuildError::ConflictingCapability {
            field: "workspaceSymbolProvider",
        };
        match &self.workspace_symbols.options {
            None if self.workspace_symbols.resolve => return Err(symbol_clash()),
            Some(options)
                if self.workspace_symbols.resolve && options.resolve_provider == Some(false) =>
            {
                return Err(symbol_clash());
            }
            _ => {}
        }

        let rename_clash = || BuildError::ConflictingCapability {
            field: "renameProvider",
        };
        match &self.rename.options {
            None if self.rename.prepare => return Err(rename_clash()),
            Some(options) if self.rename.prepare && options.prepare_provider == Some(false) => {
                return Err(rename_clash());
            }
            _ => {}
        }

        let code_action_clash = || BuildError::ConflictingCapability {
            field: "codeActionProvider",
        };
        match &self.code_actions.options {
            None if self.code_actions.resolve => return Err(code_action_clash()),
            Some(options)
                if self.code_actions.resolve && options.resolve_provider == Some(false) =>
            {
                return Err(code_action_clash());
            }
            _ => {}
        }

        let code_lens_clash = || BuildError::ConflictingCapability {
            field: "codeLensProvider",
        };
        match &self.code_lens.options {
            None if self.code_lens.resolve => return Err(code_lens_clash()),
            Some(options) if self.code_lens.resolve && options.resolve_provider == Some(false) => {
                return Err(code_lens_clash());
            }
            _ => {}
        }

        let document_link_clash = || BuildError::ConflictingCapability {
            field: "documentLinkProvider",
        };
        match &self.document_links.options {
            None if self.document_links.resolve => return Err(document_link_clash()),
            Some(options)
                if self.document_links.resolve && options.resolve_provider == Some(false) =>
            {
                return Err(document_link_clash());
            }
            _ => {}
        }

        let inlay_hint_clash = || BuildError::ConflictingCapability {
            field: "inlayHintProvider",
        };
        match &self.inlay_hints.options {
            None if self.inlay_hints.resolve => return Err(inlay_hint_clash()),
            Some(options)
                if self.inlay_hints.resolve && options.resolve_provider == Some(false) =>
            {
                return Err(inlay_hint_clash());
            }
            _ => {}
        }

        Ok(())
    }

    /// Add one command name to the execute-command capability. Duplicate names
    /// are rejected earlier as a [`BuildError::DuplicateCommand`]; the ordered
    /// list only guarantees the emitted list is de-duplicated and preserves
    /// registration order (ADR 0022).
    pub(crate) fn add_command(&mut self, name: String) {
        if !self.commands.contains(&name) {
            self.commands.push(name);
        }
    }

    /// Freeze the accumulated contributions into a `ServerCapabilities`.
    ///
    /// Each resolve/prepare family (completion, workspace symbol, rename,
    /// code action, code lens, document link, inlay hint) folds its dependent
    /// flag into the base options as `resolveProvider` or `prepareProvider`,
    /// emitting one merged provider capability per family. The diagnostics
    /// family emits its shared options as one `diagnosticProvider`. Each
    /// file-operation family emits its shared filters under exactly the
    /// `will*`/`did*` sides that registered, merged into one
    /// `workspace.fileOperations` object. The execute-command field appears
    /// only when at least one command was registered, and its command list
    /// exactly matches the frozen registry: de-duplicated and in registration
    /// order (ADR 0022).
    #[cfg(test)]
    pub(crate) fn finish(self) -> ServerCapabilities {
        self.finish_generated()
    }

    pub(crate) fn finish_generated(mut self) -> GeneratedCapabilities {
        if let Some(options) = self.call_hierarchy.options {
            self.caps.call_hierarchy_provider = Some(options.into());
        }
        if let Some(options) = self.color.options {
            self.caps.color_provider = Some(options.into());
        }
        if let Some(options) = self.semantic_tokens.finish() {
            self.caps.semantic_tokens_provider = Some(options.into());
        }
        if let Some(mut options) = self.completion.options {
            if self.completion.resolve {
                options.resolve_provider = Some(true);
            }
            self.caps.completion_provider = Some(options);
        }
        if let Some(mut options) = self.workspace_symbols.options {
            if self.workspace_symbols.resolve {
                options.resolve_provider = Some(true);
            }
            self.caps.workspace_symbol_provider = Some(options.into());
        }
        if let Some(mut options) = self.rename.options {
            if self.rename.prepare {
                options.prepare_provider = Some(true);
            }
            self.caps.rename_provider = Some(options.into());
        }
        if let Some(mut options) = self.code_actions.options {
            if self.code_actions.resolve {
                options.resolve_provider = Some(true);
            }
            self.caps.code_action_provider = Some(options.into());
        }
        if let Some(mut options) = self.code_lens.options {
            if self.code_lens.resolve {
                options.resolve_provider = Some(true);
            }
            self.caps.code_lens_provider = Some(options);
        }
        if let Some(mut options) = self.document_links.options {
            if self.document_links.resolve {
                options.resolve_provider = Some(true);
            }
            self.caps.document_link_provider = Some(options);
        }
        if let Some(mut options) = self.inlay_hints.options {
            if self.inlay_hints.resolve {
                options.resolve_provider = Some(true);
            }
            self.caps.inlay_hint_provider = Some(options.into());
        }
        if !self.commands.is_empty() {
            self.caps.execute_command_provider = Some(ExecuteCommandOptions {
                commands: self.commands,
                work_done_progress_options: Default::default(),
            });
        }
        if let Some(options) = self.diagnostics.options {
            self.caps.diagnostic_provider = Some(options.into());
        }
        // Each file-operation family advertises exactly the sides that
        // registered, with both sides of a family carrying the family's one
        // shared filters value. The `workspace` object appears only when at
        // least one side registered; the engine layers its protocol-owned
        // `workspaceFolders` field on beside this without overwriting it.
        if [&self.file_create, &self.file_rename, &self.file_delete]
            .iter()
            .any(|family| family.will || family.did)
        {
            let file_operations = FileOperationOptions {
                did_create: self.file_create.did_options(),
                will_create: self.file_create.will_options(),
                did_rename: self.file_rename.did_options(),
                will_rename: self.file_rename.will_options(),
                did_delete: self.file_delete.did_options(),
                will_delete: self.file_delete.will_options(),
            };
            self.caps.workspace = Some(WorkspaceOptions {
                file_operations: Some(file_operations),
                ..WorkspaceOptions::default()
            });
        }
        self.caps.type_hierarchy_provider = self.type_hierarchy.options.map(Into::into);
        self.caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gen_lsp_types::{
        CodeActionKind, CodeActionProvider, DeclarationOptions, DefinitionOptions,
        DiagnosticProvider, DocumentHighlightOptions, DocumentOnTypeFormattingOptions,
        DocumentSymbolOptions, HoverProvider, ImplementationRegistrationOptions,
        LinkedEditingRangeOptions, MonikerOptions, ReferenceOptions, SemanticTokensProvider,
        SignatureHelpOptions, StaticRegistrationOptions, TypeDefinitionRegistrationOptions,
        WorkDoneProgressOptions,
    };

    fn progress(value: Option<bool>) -> WorkDoneProgressOptions {
        WorkDoneProgressOptions {
            work_done_progress: value,
        }
    }

    #[test]
    fn editing_features_set_only_their_provider_fields() {
        let formatting = DocumentFormattingOptions {
            work_done_progress_options: progress(Some(true)),
        };
        let mut caps = CapabilityBuilder::default();
        caps.set_document_formatting(formatting).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                document_formatting_provider: Some(formatting.into()),
                ..ServerCapabilities::default()
            }
        );

        let range = DocumentRangeFormattingOptions {
            work_done_progress_options: progress(Some(false)),
            ranges_support: None,
        };
        let mut caps = CapabilityBuilder::default();
        caps.set_range_formatting(range).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                document_range_formatting_provider: Some(range.into()),
                ..ServerCapabilities::default()
            }
        );

        let on_type = DocumentOnTypeFormattingOptions {
            first_trigger_character: "}".to_string(),
            more_trigger_character: Some(vec![";".to_string()]),
        };
        let mut caps = CapabilityBuilder::default();
        caps.set_on_type_formatting(on_type.clone()).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                document_on_type_formatting_provider: Some(on_type),
                ..ServerCapabilities::default()
            }
        );
    }

    #[test]
    fn presentation_features_set_only_their_provider_fields() {
        let mut color = CapabilityBuilder::default();
        color.set_color_presentation();
        color
            .set_document_color(DocumentColorOptions::default())
            .unwrap();
        color.validate().unwrap();
        assert_eq!(
            color.finish(),
            ServerCapabilities {
                color_provider: Some(DocumentColorOptions::default().into()),
                ..ServerCapabilities::default()
            }
        );

        let mut folding = CapabilityBuilder::default();
        folding
            .set_folding_range(FoldingRangeOptions::default())
            .unwrap();
        assert_eq!(
            folding.finish(),
            ServerCapabilities {
                folding_range_provider: Some(FoldingRangeOptions::default().into()),
                ..ServerCapabilities::default()
            }
        );

        let selection_options = SelectionRangeOptions {
            work_done_progress_options: progress(Some(true)),
        };
        let mut selection = CapabilityBuilder::default();
        selection.set_selection_range(selection_options).unwrap();
        assert_eq!(
            selection.finish(),
            ServerCapabilities {
                selection_range_provider: Some(selection_options.into()),
                ..ServerCapabilities::default()
            }
        );

        let inline_options = InlineValueOptions {
            work_done_progress_options: progress(Some(false)),
        };
        let mut inline = CapabilityBuilder::default();
        inline.set_inline_value(inline_options).unwrap();
        assert_eq!(
            inline.finish(),
            ServerCapabilities {
                inline_value_provider: Some(inline_options.into()),
                ..ServerCapabilities::default()
            }
        );
    }

    #[test]
    fn editing_and_presentation_singular_options_never_use_last_write_wins() {
        let expected = |field| BuildError::ConflictingCapability { field };

        let mut formatting = CapabilityBuilder::default();
        formatting
            .set_document_formatting(DocumentFormattingOptions {
                work_done_progress_options: progress(None),
            })
            .unwrap();
        assert_eq!(
            formatting.set_document_formatting(DocumentFormattingOptions {
                work_done_progress_options: progress(Some(true)),
            }),
            Err(expected("documentFormattingProvider"))
        );

        let mut range = CapabilityBuilder::default();
        range
            .set_range_formatting(DocumentRangeFormattingOptions {
                work_done_progress_options: progress(None),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            range.set_range_formatting(DocumentRangeFormattingOptions {
                work_done_progress_options: progress(Some(true)),
                ..Default::default()
            }),
            Err(expected("documentRangeFormattingProvider"))
        );

        let mut on_type = CapabilityBuilder::default();
        on_type
            .set_on_type_formatting(DocumentOnTypeFormattingOptions {
                first_trigger_character: "}".into(),
                more_trigger_character: None,
            })
            .unwrap();
        assert_eq!(
            on_type.set_on_type_formatting(DocumentOnTypeFormattingOptions {
                first_trigger_character: ";".into(),
                more_trigger_character: None,
            }),
            Err(expected("documentOnTypeFormattingProvider"))
        );

        let mut color = CapabilityBuilder::default();
        color
            .set_document_color(DocumentColorOptions::default())
            .unwrap();
        color
            .set_document_color(DocumentColorOptions::default())
            .unwrap();

        let mut selection = CapabilityBuilder::default();
        selection
            .set_selection_range(SelectionRangeOptions {
                work_done_progress_options: progress(None),
            })
            .unwrap();
        assert_eq!(
            selection.set_selection_range(SelectionRangeOptions {
                work_done_progress_options: progress(Some(true)),
            }),
            Err(expected("selectionRangeProvider"))
        );

        let mut inline = CapabilityBuilder::default();
        inline
            .set_inline_value(InlineValueOptions {
                work_done_progress_options: progress(None),
            })
            .unwrap();
        assert_eq!(
            inline.set_inline_value(InlineValueOptions {
                work_done_progress_options: progress(Some(true)),
            }),
            Err(expected("inlineValueProvider"))
        );
    }

    #[test]
    fn color_presentation_without_document_color_conflicts() {
        let mut caps = CapabilityBuilder::default();
        caps.set_color_presentation();
        assert_eq!(
            caps.validate(),
            Err(BuildError::ConflictingCapability {
                field: "colorProvider"
            })
        );
    }

    #[test]
    fn hover_sets_only_hover_provider() {
        let mut caps = CapabilityBuilder::default();
        caps.set_hover().unwrap();
        let caps = caps.finish();
        assert_eq!(caps.hover_provider, Some(HoverProvider::Bool(true)));
        assert_eq!(caps.completion_provider, None);
        assert_eq!(caps.execute_command_provider, None);
    }

    #[test]
    fn completion_advertises_supplied_options() {
        let options = CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..CompletionOptions::default()
        };
        let mut caps = CapabilityBuilder::default();
        caps.set_completion(options.clone()).unwrap();
        assert_eq!(caps.finish().completion_provider, Some(options));
    }

    #[test]
    fn identical_completion_contributions_merge() {
        let options = CompletionOptions::default();
        let mut caps = CapabilityBuilder::default();
        caps.set_completion(options.clone()).unwrap();
        caps.set_completion(options.clone())
            .expect("identical options merge without conflict");
        assert_eq!(caps.finish().completion_provider, Some(options));
    }

    #[test]
    fn completion_resolve_sets_resolve_provider_on_the_base_capability() {
        let options = CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..CompletionOptions::default()
        };
        let mut caps = CapabilityBuilder::default();
        caps.set_completion(options).unwrap();
        caps.set_completion_resolve();
        let merged = caps
            .finish()
            .completion_provider
            .expect("completion contributes a completionProvider capability");
        assert_eq!(
            merged.resolve_provider,
            Some(true),
            "the resolve contribution augments the same capability, not a second one"
        );
        assert_eq!(
            merged.trigger_characters,
            Some(vec![".".to_string()]),
            "the base feature's options survive the family merge"
        );
    }

    #[test]
    fn completion_resolve_merge_is_independent_of_registration_order() {
        let options = || CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..CompletionOptions::default()
        };
        let mut base_first = CapabilityBuilder::default();
        base_first.set_completion(options()).unwrap();
        base_first.set_completion_resolve();

        let mut resolve_first = CapabilityBuilder::default();
        resolve_first.set_completion_resolve();
        resolve_first.set_completion(options()).unwrap();

        assert_eq!(
            base_first.finish().completion_provider,
            resolve_first.finish().completion_provider,
            "the merged capability does not depend on which feature registered first"
        );
    }

    #[test]
    fn completion_resolve_without_completion_fails_validation() {
        let mut caps = CapabilityBuilder::default();
        caps.set_completion_resolve();
        let err = caps.validate().expect_err(
            "resolve without its base feature must not advertise a dangling capability",
        );
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "completionProvider"
            }
        );
    }

    #[test]
    fn a_base_that_denies_resolve_clashes_with_a_resolve_contribution() {
        let mut caps = CapabilityBuilder::default();
        caps.set_completion(CompletionOptions {
            resolve_provider: Some(false),
            ..CompletionOptions::default()
        })
        .unwrap();
        caps.set_completion_resolve();
        let err = caps
            .validate()
            .expect_err("a base denying resolve and a resolve contribution must clash");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "completionProvider"
            }
        );
    }

    #[test]
    fn disagreeing_completion_contributions_conflict() {
        let mut caps = CapabilityBuilder::default();
        caps.set_completion(CompletionOptions::default()).unwrap();
        let err = caps
            .set_completion(CompletionOptions {
                trigger_characters: Some(vec![".".to_string()]),
                ..CompletionOptions::default()
            })
            .expect_err("differing completion options must not last-write-win");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "completionProvider"
            }
        );
    }

    fn diagnostic_options(
        identifier: Option<&str>,
        workspace_diagnostics: bool,
    ) -> DiagnosticOptions {
        DiagnosticOptions {
            identifier: identifier.map(str::to_string),
            inter_file_dependencies: true,
            workspace_diagnostics,
            work_done_progress_options: Default::default(),
        }
    }

    #[test]
    fn identical_diagnostic_contributions_merge() {
        let options = diagnostic_options(Some("compiler"), true);
        let mut caps = CapabilityBuilder::default();
        caps.set_diagnostics(options.clone()).unwrap();
        caps.set_diagnostics(options.clone()).unwrap();
        assert_eq!(
            caps.finish().diagnostic_provider,
            Some(DiagnosticProvider::DiagnosticOptions(options))
        );
    }

    #[test]
    fn conflicting_diagnostic_identifier_is_rejected() {
        let mut caps = CapabilityBuilder::default();
        caps.set_diagnostics(diagnostic_options(Some("compiler"), true))
            .unwrap();
        let error = caps
            .set_diagnostics(diagnostic_options(Some("linter"), true))
            .expect_err("identifier drift must conflict");
        assert_eq!(
            error,
            BuildError::ConflictingCapability {
                field: "diagnosticProvider"
            }
        );
    }

    #[test]
    fn conflicting_workspace_diagnostics_setting_is_rejected() {
        let mut caps = CapabilityBuilder::default();
        caps.set_diagnostics(diagnostic_options(Some("compiler"), false))
            .unwrap();
        let error = caps
            .set_diagnostics(diagnostic_options(Some("compiler"), true))
            .expect_err("workspaceDiagnostics drift must conflict");
        assert_eq!(
            error,
            BuildError::ConflictingCapability {
                field: "diagnosticProvider"
            }
        );
    }

    #[test]
    fn commands_merge_into_one_deduplicated_registration_order_list() {
        let mut caps = CapabilityBuilder::default();
        for name in ["b.cmd", "a.cmd", "b.cmd"] {
            caps.add_command(name.to_string());
        }
        let provider = caps
            .finish()
            .execute_command_provider
            .expect("registered commands contribute an execute-command capability");
        assert_eq!(
            provider.commands,
            vec!["b.cmd".to_string(), "a.cmd".to_string()],
            "commands are de-duplicated and keep registration order, not sorted order"
        );
    }

    #[test]
    fn no_contributions_yield_default_capabilities() {
        assert_eq!(
            CapabilityBuilder::default().finish(),
            ServerCapabilities::default()
        );
    }

    fn workspace_symbol_options(resolve_provider: Option<bool>) -> WorkspaceSymbolOptions {
        WorkspaceSymbolOptions {
            work_done_progress_options: Default::default(),
            resolve_provider,
        }
    }

    #[test]
    fn workspace_symbol_advertises_supplied_options() {
        let options = workspace_symbol_options(None);
        let mut caps = CapabilityBuilder::default();
        caps.set_workspace_symbol(options).unwrap();
        assert_eq!(
            caps.finish().workspace_symbol_provider,
            Some(options.into())
        );
    }

    #[test]
    fn workspace_symbol_resolve_merge_is_independent_of_registration_order() {
        let mut base_first = CapabilityBuilder::default();
        base_first
            .set_workspace_symbol(workspace_symbol_options(None))
            .unwrap();
        base_first.set_workspace_symbol_resolve();

        let mut resolve_first = CapabilityBuilder::default();
        resolve_first.set_workspace_symbol_resolve();
        resolve_first
            .set_workspace_symbol(workspace_symbol_options(None))
            .unwrap();

        assert_eq!(
            base_first.finish().workspace_symbol_provider,
            resolve_first.finish().workspace_symbol_provider,
            "the merged capability does not depend on which feature registered first"
        );
        let mut merged = CapabilityBuilder::default();
        merged
            .set_workspace_symbol(workspace_symbol_options(None))
            .unwrap();
        merged.set_workspace_symbol_resolve();
        let merged = merged
            .finish()
            .workspace_symbol_provider
            .expect("the family emits one workspaceSymbolProvider capability");
        assert_eq!(
            merged,
            workspace_symbol_options(Some(true)).into(),
            "the resolve contribution augments the same capability, not a second one"
        );
    }

    #[test]
    fn workspace_symbol_resolve_without_workspace_symbol_fails_validation() {
        let mut caps = CapabilityBuilder::default();
        caps.set_workspace_symbol_resolve();
        let err = caps.validate().expect_err(
            "resolve without its base feature must not advertise a dangling capability",
        );
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "workspaceSymbolProvider"
            }
        );
    }

    #[test]
    fn a_workspace_symbol_base_that_denies_resolve_clashes_with_a_resolve_contribution() {
        let mut caps = CapabilityBuilder::default();
        caps.set_workspace_symbol(workspace_symbol_options(Some(false)))
            .unwrap();
        caps.set_workspace_symbol_resolve();
        let err = caps
            .validate()
            .expect_err("a base denying resolve and a resolve contribution must clash");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "workspaceSymbolProvider"
            }
        );
    }

    #[test]
    fn disagreeing_workspace_symbol_contributions_conflict() {
        let mut caps = CapabilityBuilder::default();
        caps.set_workspace_symbol(workspace_symbol_options(None))
            .unwrap();
        let err = caps
            .set_workspace_symbol(WorkspaceSymbolOptions {
                work_done_progress_options: gen_lsp_types::WorkDoneProgressOptions {
                    work_done_progress: Some(true),
                },
                resolve_provider: None,
            })
            .expect_err("differing workspace-symbol options must not last-write-win");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "workspaceSymbolProvider"
            }
        );
    }

    fn file_operation_options(glob: &str) -> FileOperationRegistrationOptions {
        FileOperationRegistrationOptions {
            filters: vec![gen_lsp_types::FileOperationFilter {
                scheme: Some("file".to_string()),
                pattern: gen_lsp_types::FileOperationPattern {
                    glob: glob.to_string(),
                    matches: Some(gen_lsp_types::FileOperationPatternKind::File),
                    options: None,
                },
            }],
        }
    }

    #[test]
    fn will_and_did_sides_of_a_family_merge_into_shared_filters() {
        let mut caps = CapabilityBuilder::default();
        caps.set_will_create(file_operation_options("**/*.rs"))
            .unwrap();
        caps.set_did_create(file_operation_options("**/*.rs"))
            .unwrap();
        let file_operations = caps
            .finish()
            .workspace
            .expect("a registered file operation advertises the workspace object")
            .file_operations
            .expect("the family advertises a fileOperations capability");
        let expected = Some(file_operation_options("**/*.rs"));
        assert_eq!(file_operations.will_create, expected.clone());
        assert_eq!(
            file_operations.did_create, expected,
            "both sides of the family carry the one shared filters value"
        );
        assert_eq!(file_operations.will_rename, None);
        assert_eq!(file_operations.did_rename, None);
        assert_eq!(file_operations.will_delete, None);
        assert_eq!(file_operations.did_delete, None);
    }

    #[test]
    fn file_operation_merge_is_independent_of_registration_order() {
        let mut will_first = CapabilityBuilder::default();
        will_first
            .set_will_rename(file_operation_options("**/*.rs"))
            .unwrap();
        will_first
            .set_did_rename(file_operation_options("**/*.rs"))
            .unwrap();

        let mut did_first = CapabilityBuilder::default();
        did_first
            .set_did_rename(file_operation_options("**/*.rs"))
            .unwrap();
        did_first
            .set_will_rename(file_operation_options("**/*.rs"))
            .unwrap();

        assert_eq!(
            will_first.finish().workspace,
            did_first.finish().workspace,
            "the family merge does not depend on which side registered first"
        );
    }

    #[test]
    fn a_will_only_family_advertises_no_did_side() {
        let mut caps = CapabilityBuilder::default();
        caps.set_will_delete(file_operation_options("**/*.tmp"))
            .unwrap();
        let file_operations = caps
            .finish()
            .workspace
            .expect("a will-only family still advertises the workspace object")
            .file_operations
            .expect("the family advertises a fileOperations capability");
        assert_eq!(
            file_operations.will_delete,
            Some(file_operation_options("**/*.tmp"))
        );
        assert_eq!(file_operations.did_delete, None);
    }

    #[test]
    fn disagreeing_filters_within_a_family_conflict() {
        let mut caps = CapabilityBuilder::default();
        caps.set_will_create(file_operation_options("**/*.rs"))
            .unwrap();
        let err = caps
            .set_did_create(file_operation_options("**/*.toml"))
            .expect_err("differing filters within one family must not last-write-win");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "workspace.fileOperations.create"
            }
        );
    }

    #[test]
    fn disagreeing_filters_in_another_family_conflict_deterministically() {
        let mut caps = CapabilityBuilder::default();
        caps.set_did_rename(file_operation_options("**/*.rs"))
            .unwrap();
        let err = caps
            .set_will_rename(file_operation_options("**/*.toml"))
            .expect_err("the rename family rejects drifting filters as well");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "workspace.fileOperations.rename"
            }
        );
    }

    #[test]
    fn no_file_operations_yield_no_workspace_capability() {
        let mut caps = CapabilityBuilder::default();
        caps.set_hover().unwrap();
        assert_eq!(caps.finish().workspace, None);
    }

    fn rename_options(prepare_provider: Option<bool>) -> RenameOptions {
        RenameOptions {
            prepare_provider,
            work_done_progress_options: Default::default(),
        }
    }

    #[test]
    fn rename_advertises_supplied_options() {
        let options = rename_options(None);
        let mut caps = CapabilityBuilder::default();
        caps.set_rename(options).unwrap();
        assert_eq!(caps.finish().rename_provider, Some(options.into()));
    }

    #[test]
    fn prepare_rename_merge_is_independent_of_registration_order() {
        let mut base_first = CapabilityBuilder::default();
        base_first.set_rename(rename_options(None)).unwrap();
        base_first.set_prepare_rename();

        let mut prepare_first = CapabilityBuilder::default();
        prepare_first.set_prepare_rename();
        prepare_first.set_rename(rename_options(None)).unwrap();

        assert_eq!(
            base_first.finish().rename_provider,
            prepare_first.finish().rename_provider,
            "the merged capability does not depend on which feature registered first"
        );
        let mut merged = CapabilityBuilder::default();
        merged.set_rename(rename_options(None)).unwrap();
        merged.set_prepare_rename();
        let merged = merged
            .finish()
            .rename_provider
            .expect("the family emits one renameProvider capability");
        assert_eq!(
            merged,
            rename_options(Some(true)).into(),
            "the prepare contribution augments the same capability, not a second one"
        );
    }

    #[test]
    fn prepare_rename_without_rename_fails_validation() {
        let mut caps = CapabilityBuilder::default();
        caps.set_prepare_rename();
        let err = caps.validate().expect_err(
            "prepare without its base feature must not advertise a dangling capability",
        );
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "renameProvider"
            }
        );
    }

    #[test]
    fn a_rename_base_that_denies_prepare_clashes_with_a_prepare_contribution() {
        let mut caps = CapabilityBuilder::default();
        caps.set_rename(rename_options(Some(false))).unwrap();
        caps.set_prepare_rename();
        let err = caps
            .validate()
            .expect_err("a base denying prepare and a prepare contribution must clash");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "renameProvider"
            }
        );
    }

    #[test]
    fn disagreeing_rename_contributions_conflict() {
        let mut caps = CapabilityBuilder::default();
        caps.set_rename(rename_options(None)).unwrap();
        let err = caps
            .set_rename(rename_options(Some(true)))
            .expect_err("differing rename options must not last-write-win");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "renameProvider"
            }
        );
    }

    fn code_action_options(resolve_provider: Option<bool>) -> CodeActionOptions {
        CodeActionOptions {
            code_action_kinds: Some(vec![CodeActionKind::QuickFix]),
            work_done_progress_options: Default::default(),
            resolve_provider,
            ..Default::default()
        }
    }

    #[test]
    fn code_action_advertises_supplied_options() {
        let options = code_action_options(None);
        let mut caps = CapabilityBuilder::default();
        caps.set_code_action(options.clone()).unwrap();
        assert_eq!(
            caps.finish().code_action_provider,
            Some(CodeActionProvider::CodeActionOptions(options))
        );
    }

    #[test]
    fn code_action_resolve_merge_is_independent_of_registration_order() {
        let mut base_first = CapabilityBuilder::default();
        base_first
            .set_code_action(code_action_options(None))
            .unwrap();
        base_first.set_code_action_resolve();

        let mut resolve_first = CapabilityBuilder::default();
        resolve_first.set_code_action_resolve();
        resolve_first
            .set_code_action(code_action_options(None))
            .unwrap();

        assert_eq!(
            base_first.finish().code_action_provider,
            resolve_first.finish().code_action_provider,
            "the merged capability does not depend on which feature registered first"
        );
        let mut merged = CapabilityBuilder::default();
        merged.set_code_action(code_action_options(None)).unwrap();
        merged.set_code_action_resolve();
        let merged = merged
            .finish()
            .code_action_provider
            .expect("the family emits one codeActionProvider capability");
        assert_eq!(
            merged,
            CodeActionProvider::CodeActionOptions(code_action_options(Some(true))),
            "the resolve contribution augments the same capability, not a second one"
        );
    }

    #[test]
    fn code_action_resolve_without_code_action_fails_validation() {
        let mut caps = CapabilityBuilder::default();
        caps.set_code_action_resolve();
        let err = caps.validate().expect_err(
            "resolve without its base feature must not advertise a dangling capability",
        );
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "codeActionProvider"
            }
        );
    }

    #[test]
    fn a_code_action_base_that_denies_resolve_clashes_with_a_resolve_contribution() {
        let mut caps = CapabilityBuilder::default();
        caps.set_code_action(code_action_options(Some(false)))
            .unwrap();
        caps.set_code_action_resolve();
        let err = caps
            .validate()
            .expect_err("a base denying resolve and a resolve contribution must clash");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "codeActionProvider"
            }
        );
    }

    #[test]
    fn disagreeing_code_action_contributions_conflict() {
        let mut caps = CapabilityBuilder::default();
        caps.set_code_action(code_action_options(None)).unwrap();
        let err = caps
            .set_code_action(CodeActionOptions::default())
            .expect_err("differing code-action options must not last-write-win");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "codeActionProvider"
            }
        );
    }

    fn code_lens_options(resolve_provider: Option<bool>) -> CodeLensOptions {
        CodeLensOptions {
            resolve_provider,
            ..Default::default()
        }
    }

    #[test]
    fn code_lens_advertises_supplied_options() {
        let options = code_lens_options(None);
        let mut caps = CapabilityBuilder::default();
        caps.set_code_lens(options).unwrap();
        assert_eq!(caps.finish().code_lens_provider, Some(options));
    }

    #[test]
    fn code_lens_resolve_merge_is_independent_of_registration_order() {
        let mut base_first = CapabilityBuilder::default();
        base_first.set_code_lens(code_lens_options(None)).unwrap();
        base_first.set_code_lens_resolve();

        let mut resolve_first = CapabilityBuilder::default();
        resolve_first.set_code_lens_resolve();
        resolve_first
            .set_code_lens(code_lens_options(None))
            .unwrap();

        assert_eq!(
            base_first.finish().code_lens_provider,
            resolve_first.finish().code_lens_provider,
            "the merged capability does not depend on which feature registered first"
        );
        let mut merged = CapabilityBuilder::default();
        merged.set_code_lens(code_lens_options(None)).unwrap();
        merged.set_code_lens_resolve();
        let merged = merged
            .finish()
            .code_lens_provider
            .expect("the family emits one codeLensProvider capability");
        assert_eq!(
            merged,
            code_lens_options(Some(true)),
            "the resolve contribution augments the same capability, not a second one"
        );
    }

    #[test]
    fn code_lens_resolve_without_code_lens_fails_validation() {
        let mut caps = CapabilityBuilder::default();
        caps.set_code_lens_resolve();
        let err = caps.validate().expect_err(
            "resolve without its base feature must not advertise a dangling capability",
        );
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "codeLensProvider"
            }
        );
    }

    #[test]
    fn a_code_lens_base_that_denies_resolve_clashes_with_a_resolve_contribution() {
        let mut caps = CapabilityBuilder::default();
        caps.set_code_lens(code_lens_options(Some(false))).unwrap();
        caps.set_code_lens_resolve();
        let err = caps
            .validate()
            .expect_err("a base denying resolve and a resolve contribution must clash");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "codeLensProvider"
            }
        );
    }

    #[test]
    fn disagreeing_code_lens_contributions_conflict() {
        let mut caps = CapabilityBuilder::default();
        caps.set_code_lens(code_lens_options(Some(true))).unwrap();
        let err = caps
            .set_code_lens(code_lens_options(None))
            .expect_err("differing code-lens options must not last-write-win");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "codeLensProvider"
            }
        );
    }

    fn document_link_options(resolve_provider: Option<bool>) -> DocumentLinkOptions {
        DocumentLinkOptions {
            resolve_provider,
            work_done_progress_options: Default::default(),
        }
    }

    #[test]
    fn document_link_advertises_supplied_options() {
        let options = document_link_options(None);
        let mut caps = CapabilityBuilder::default();
        caps.set_document_link(options).unwrap();
        assert_eq!(caps.finish().document_link_provider, Some(options));
    }

    #[test]
    fn document_link_resolve_merge_is_independent_of_registration_order() {
        let mut base_first = CapabilityBuilder::default();
        base_first
            .set_document_link(document_link_options(None))
            .unwrap();
        base_first.set_document_link_resolve();

        let mut resolve_first = CapabilityBuilder::default();
        resolve_first.set_document_link_resolve();
        resolve_first
            .set_document_link(document_link_options(None))
            .unwrap();

        assert_eq!(
            base_first.finish().document_link_provider,
            resolve_first.finish().document_link_provider,
            "the merged capability does not depend on which feature registered first"
        );
        let mut merged = CapabilityBuilder::default();
        merged
            .set_document_link(document_link_options(None))
            .unwrap();
        merged.set_document_link_resolve();
        let merged = merged
            .finish()
            .document_link_provider
            .expect("the family emits one documentLinkProvider capability");
        assert_eq!(
            merged,
            document_link_options(Some(true)),
            "the resolve contribution augments the same capability, not a second one"
        );
    }

    #[test]
    fn document_link_resolve_without_document_link_fails_validation() {
        let mut caps = CapabilityBuilder::default();
        caps.set_document_link_resolve();
        let err = caps.validate().expect_err(
            "resolve without its base feature must not advertise a dangling capability",
        );
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "documentLinkProvider"
            }
        );
    }

    #[test]
    fn a_document_link_base_that_denies_resolve_clashes_with_a_resolve_contribution() {
        let mut caps = CapabilityBuilder::default();
        caps.set_document_link(document_link_options(Some(false)))
            .unwrap();
        caps.set_document_link_resolve();
        let err = caps
            .validate()
            .expect_err("a base denying resolve and a resolve contribution must clash");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "documentLinkProvider"
            }
        );
    }

    #[test]
    fn disagreeing_document_link_contributions_conflict() {
        let mut caps = CapabilityBuilder::default();
        caps.set_document_link(document_link_options(None)).unwrap();
        let err = caps
            .set_document_link(DocumentLinkOptions {
                resolve_provider: None,
                work_done_progress_options: gen_lsp_types::WorkDoneProgressOptions {
                    work_done_progress: Some(true),
                },
            })
            .expect_err("differing document-link options must not last-write-win");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "documentLinkProvider"
            }
        );
    }

    fn inlay_hint_options(resolve_provider: Option<bool>) -> InlayHintOptions {
        InlayHintOptions {
            work_done_progress_options: Default::default(),
            resolve_provider,
        }
    }

    #[test]
    fn inlay_hint_advertises_supplied_options() {
        let options = inlay_hint_options(None);
        let mut caps = CapabilityBuilder::default();
        caps.set_inlay_hint(options).unwrap();
        assert_eq!(caps.finish().inlay_hint_provider, Some(options.into()));
    }

    #[test]
    fn inlay_hint_resolve_merge_is_independent_of_registration_order() {
        let mut base_first = CapabilityBuilder::default();
        base_first.set_inlay_hint(inlay_hint_options(None)).unwrap();
        base_first.set_inlay_hint_resolve();

        let mut resolve_first = CapabilityBuilder::default();
        resolve_first.set_inlay_hint_resolve();
        resolve_first
            .set_inlay_hint(inlay_hint_options(None))
            .unwrap();

        assert_eq!(
            base_first.finish().inlay_hint_provider,
            resolve_first.finish().inlay_hint_provider,
            "the merged capability does not depend on which feature registered first"
        );
        let mut merged = CapabilityBuilder::default();
        merged.set_inlay_hint(inlay_hint_options(None)).unwrap();
        merged.set_inlay_hint_resolve();
        let merged = merged
            .finish()
            .inlay_hint_provider
            .expect("the family emits one inlayHintProvider capability");
        assert_eq!(
            merged,
            inlay_hint_options(Some(true)).into(),
            "the resolve contribution augments the same capability, not a second one"
        );
    }

    #[test]
    fn inlay_hint_resolve_without_inlay_hint_fails_validation() {
        let mut caps = CapabilityBuilder::default();
        caps.set_inlay_hint_resolve();
        let err = caps.validate().expect_err(
            "resolve without its base feature must not advertise a dangling capability",
        );
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "inlayHintProvider"
            }
        );
    }

    #[test]
    fn an_inlay_hint_base_that_denies_resolve_clashes_with_a_resolve_contribution() {
        let mut caps = CapabilityBuilder::default();
        caps.set_inlay_hint(inlay_hint_options(Some(false)))
            .unwrap();
        caps.set_inlay_hint_resolve();
        let err = caps
            .validate()
            .expect_err("a base denying resolve and a resolve contribution must clash");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "inlayHintProvider"
            }
        );
    }

    #[test]
    fn disagreeing_inlay_hint_contributions_conflict() {
        let mut caps = CapabilityBuilder::default();
        caps.set_inlay_hint(inlay_hint_options(None)).unwrap();
        let err = caps
            .set_inlay_hint(InlayHintOptions {
                work_done_progress_options: gen_lsp_types::WorkDoneProgressOptions {
                    work_done_progress: Some(true),
                },
                resolve_provider: None,
            })
            .expect_err("differing inlay-hint options must not last-write-win");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "inlayHintProvider"
            }
        );
    }

    #[test]
    fn call_hierarchy_routes_merge_into_one_prepare_capability() {
        let options = CallHierarchyOptions {
            work_done_progress_options: gen_lsp_types::WorkDoneProgressOptions {
                work_done_progress: Some(true),
            },
        };
        let mut caps = CapabilityBuilder::default();
        caps.set_call_hierarchy_incoming_calls();
        caps.set_call_hierarchy(options).unwrap();
        caps.set_call_hierarchy_outgoing_calls();
        caps.validate().unwrap();

        assert_eq!(caps.finish().call_hierarchy_provider, Some(options.into()));
    }

    #[test]
    fn call_hierarchy_subordinate_without_prepare_conflicts() {
        let mut caps = CapabilityBuilder::default();
        caps.set_call_hierarchy_incoming_calls();

        assert_eq!(
            caps.validate(),
            Err(BuildError::ConflictingCapability {
                field: "callHierarchyProvider"
            })
        );
    }

    #[test]
    fn type_hierarchy_routes_share_the_prepare_options() {
        let options = TypeHierarchyOptions {
            work_done_progress_options: gen_lsp_types::WorkDoneProgressOptions {
                work_done_progress: Some(true),
            },
        };
        let mut caps = CapabilityBuilder::default();
        caps.set_type_hierarchy_subtypes();
        caps.set_type_hierarchy(options).unwrap();
        caps.set_type_hierarchy_supertypes();
        caps.validate().unwrap();

        assert_eq!(
            caps.finish_generated().type_hierarchy_provider,
            Some(options.into())
        );
    }

    #[test]
    fn type_hierarchy_subordinate_without_prepare_conflicts() {
        let mut caps = CapabilityBuilder::default();
        caps.set_type_hierarchy_supertypes();

        assert_eq!(
            caps.validate(),
            Err(BuildError::ConflictingCapability {
                field: "typeHierarchyProvider"
            })
        );
    }

    fn semantic_options() -> SemanticTokensOptions {
        SemanticTokensOptions {
            work_done_progress_options: gen_lsp_types::WorkDoneProgressOptions {
                work_done_progress: Some(true),
            },
            legend: gen_lsp_types::SemanticTokensLegend {
                token_types: vec!["keyword".to_string()],
                token_modifiers: vec![],
            },
            range: None,
            full: None,
        }
    }

    #[test]
    fn semantic_token_routes_merge_shared_options_and_modes() {
        let mut caps = CapabilityBuilder::default();
        let mut full = semantic_options();
        full.full = Some(Full::Bool(true));
        caps.set_semantic_tokens_full(full).unwrap();
        let mut delta = semantic_options();
        delta.full = Some(Full::SemanticTokensFullDelta(SemanticTokensFullDelta {
            delta: Some(true),
        }));
        caps.set_semantic_tokens_full_delta(delta).unwrap();
        let mut range = semantic_options();
        range.range = Some(true.into());
        caps.set_semantic_tokens_range(range).unwrap();
        caps.validate().unwrap();

        let provider = caps
            .finish()
            .semantic_tokens_provider
            .expect("one semanticTokensProvider");
        let SemanticTokensProvider::SemanticTokensOptions(options) = provider else {
            panic!("static features emit plain semantic-token options")
        };
        assert_eq!(options.legend, semantic_options().legend);
        assert_eq!(options.range, Some(true.into()));
        assert_eq!(
            options.full,
            Some(Full::SemanticTokensFullDelta(SemanticTokensFullDelta {
                delta: Some(true),
            }))
        );
    }

    #[test]
    fn semantic_token_delta_without_full_conflicts() {
        let mut caps = CapabilityBuilder::default();
        caps.set_semantic_tokens_full_delta(semantic_options())
            .unwrap();

        assert_eq!(
            caps.validate(),
            Err(BuildError::ConflictingCapability {
                field: "semanticTokensProvider"
            })
        );
    }

    #[test]
    fn semantic_token_legend_mode_and_shared_option_conflicts_are_rejected() {
        let mut caps = CapabilityBuilder::default();
        caps.set_semantic_tokens_full(semantic_options()).unwrap();
        let mut different_legend = semantic_options();
        different_legend.legend.token_types = vec!["string".to_string()];
        assert_eq!(
            caps.set_semantic_tokens_range(different_legend),
            Err(BuildError::ConflictingCapability {
                field: "semanticTokensProvider"
            })
        );

        let mut shared_options = CapabilityBuilder::default();
        shared_options
            .set_semantic_tokens_full(semantic_options())
            .unwrap();
        let mut different_progress = semantic_options();
        different_progress
            .work_done_progress_options
            .work_done_progress = Some(false);
        assert_eq!(
            shared_options.set_semantic_tokens_range(different_progress),
            Err(BuildError::ConflictingCapability {
                field: "semanticTokensProvider"
            })
        );

        let mut denied = CapabilityBuilder::default();
        let mut options = semantic_options();
        options.full = Some(Full::Bool(false));
        denied.set_semantic_tokens_full(options).unwrap();
        assert_eq!(
            denied.validate(),
            Err(BuildError::ConflictingCapability {
                field: "semanticTokensProvider"
            })
        );
    }

    fn signature_options(trigger: &str) -> SignatureHelpOptions {
        SignatureHelpOptions {
            trigger_characters: Some(vec![trigger.to_string()]),
            retrigger_characters: None,
            work_done_progress_options: Default::default(),
        }
    }

    fn declaration_options() -> DeclarationOptions {
        DeclarationOptions {
            work_done_progress_options: progress(Some(true)),
        }
    }

    fn definition_options() -> DefinitionOptions {
        DefinitionOptions {
            work_done_progress_options: progress(Some(true)),
        }
    }

    fn type_definition_registration_options(id: &str) -> TypeDefinitionRegistrationOptions {
        TypeDefinitionRegistrationOptions {
            static_registration_options: StaticRegistrationOptions {
                id: Some(id.to_string()),
            },
            ..TypeDefinitionRegistrationOptions::default()
        }
    }

    fn implementation_registration_options(id: &str) -> ImplementationRegistrationOptions {
        ImplementationRegistrationOptions {
            static_registration_options: StaticRegistrationOptions {
                id: Some(id.to_string()),
            },
            ..ImplementationRegistrationOptions::default()
        }
    }

    fn references_options() -> ReferenceOptions {
        ReferenceOptions {
            work_done_progress_options: progress(Some(true)),
        }
    }

    fn document_highlight_options() -> DocumentHighlightOptions {
        DocumentHighlightOptions {
            work_done_progress_options: progress(Some(true)),
        }
    }

    fn document_symbol_options(label: &str) -> DocumentSymbolOptions {
        DocumentSymbolOptions {
            label: Some(label.to_string()),
            work_done_progress_options: progress(Some(true)),
        }
    }

    fn linked_editing_range_options() -> LinkedEditingRangeOptions {
        LinkedEditingRangeOptions {
            work_done_progress_options: progress(Some(true)),
        }
    }

    fn moniker_options() -> MonikerOptions {
        MonikerOptions {
            work_done_progress_options: progress(Some(true)),
        }
    }

    #[test]
    fn navigation_features_set_only_their_provider_fields() {
        let signature = signature_options("(");
        let mut caps = CapabilityBuilder::default();
        caps.set_signature_help(signature.clone()).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                signature_help_provider: Some(signature),
                ..ServerCapabilities::default()
            }
        );

        let declaration = declaration_options();
        let mut caps = CapabilityBuilder::default();
        caps.set_declaration(declaration).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                declaration_provider: Some(declaration.into()),
                ..ServerCapabilities::default()
            }
        );

        let definition = definition_options();
        let mut caps = CapabilityBuilder::default();
        caps.set_definition(definition).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                definition_provider: Some(definition.into()),
                ..ServerCapabilities::default()
            }
        );

        let type_definition = type_definition_registration_options("type");
        let mut caps = CapabilityBuilder::default();
        caps.set_type_definition(type_definition.clone()).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                type_definition_provider: Some(type_definition.into()),
                ..ServerCapabilities::default()
            }
        );

        let implementation = implementation_registration_options("impl");
        let mut caps = CapabilityBuilder::default();
        caps.set_implementation(implementation.clone()).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                implementation_provider: Some(implementation.into()),
                ..ServerCapabilities::default()
            }
        );
    }

    #[test]
    fn lookup_features_set_only_their_provider_fields() {
        let references = references_options();
        let mut caps = CapabilityBuilder::default();
        caps.set_references(references).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                references_provider: Some(references.into()),
                ..ServerCapabilities::default()
            }
        );

        let highlight = document_highlight_options();
        let mut caps = CapabilityBuilder::default();
        caps.set_document_highlight(highlight).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                document_highlight_provider: Some(highlight.into()),
                ..ServerCapabilities::default()
            }
        );

        let symbols = document_symbol_options("outline");
        let mut caps = CapabilityBuilder::default();
        caps.set_document_symbol(symbols.clone()).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                document_symbol_provider: Some(symbols.into()),
                ..ServerCapabilities::default()
            }
        );

        let linked = linked_editing_range_options();
        let mut caps = CapabilityBuilder::default();
        caps.set_linked_editing_range(linked).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                linked_editing_range_provider: Some(linked.into()),
                ..ServerCapabilities::default()
            }
        );

        let monikers = moniker_options();
        let mut caps = CapabilityBuilder::default();
        caps.set_moniker(monikers).unwrap();
        assert_eq!(
            caps.finish(),
            ServerCapabilities {
                moniker_provider: Some(monikers.into()),
                ..ServerCapabilities::default()
            }
        );
    }

    #[test]
    fn navigation_and_lookup_singular_options_never_use_last_write_wins() {
        let mut signature = CapabilityBuilder::default();
        signature
            .set_signature_help(signature_options("("))
            .unwrap();
        assert_eq!(
            signature.set_signature_help(signature_options("[")),
            Err(BuildError::ConflictingCapability {
                field: "signatureHelpProvider"
            })
        );

        let mut declaration = CapabilityBuilder::default();
        declaration.set_declaration(declaration_options()).unwrap();
        assert_eq!(
            declaration.set_declaration(DeclarationOptions {
                work_done_progress_options: progress(Some(false)),
            }),
            Err(BuildError::ConflictingCapability {
                field: "declarationProvider"
            })
        );

        let mut definition = CapabilityBuilder::default();
        definition.set_definition(definition_options()).unwrap();
        assert_eq!(
            definition.set_definition(DefinitionOptions {
                work_done_progress_options: progress(Some(false)),
            }),
            Err(BuildError::ConflictingCapability {
                field: "definitionProvider"
            })
        );

        let mut type_definition = CapabilityBuilder::default();
        type_definition
            .set_type_definition(type_definition_registration_options("type"))
            .unwrap();
        assert_eq!(
            type_definition.set_type_definition(type_definition_registration_options("other")),
            Err(BuildError::ConflictingCapability {
                field: "typeDefinitionProvider"
            })
        );

        let mut implementation = CapabilityBuilder::default();
        implementation
            .set_implementation(implementation_registration_options("impl"))
            .unwrap();
        assert_eq!(
            implementation.set_implementation(implementation_registration_options("other")),
            Err(BuildError::ConflictingCapability {
                field: "implementationProvider"
            })
        );

        let mut references = CapabilityBuilder::default();
        references.set_references(references_options()).unwrap();
        assert_eq!(
            references.set_references(ReferenceOptions {
                work_done_progress_options: progress(Some(false)),
            }),
            Err(BuildError::ConflictingCapability {
                field: "referencesProvider"
            })
        );

        let mut highlight = CapabilityBuilder::default();
        highlight
            .set_document_highlight(document_highlight_options())
            .unwrap();
        assert_eq!(
            highlight.set_document_highlight(DocumentHighlightOptions {
                work_done_progress_options: progress(Some(false)),
            }),
            Err(BuildError::ConflictingCapability {
                field: "documentHighlightProvider"
            })
        );

        let mut symbols = CapabilityBuilder::default();
        symbols
            .set_document_symbol(document_symbol_options("outline"))
            .unwrap();
        assert_eq!(
            symbols.set_document_symbol(document_symbol_options("other")),
            Err(BuildError::ConflictingCapability {
                field: "documentSymbolProvider"
            })
        );

        let mut linked = CapabilityBuilder::default();
        linked
            .set_linked_editing_range(linked_editing_range_options())
            .unwrap();
        assert_eq!(
            linked.set_linked_editing_range(LinkedEditingRangeOptions {
                work_done_progress_options: progress(Some(false)),
            }),
            Err(BuildError::ConflictingCapability {
                field: "linkedEditingRangeProvider"
            })
        );

        let mut moniker = CapabilityBuilder::default();
        moniker.set_moniker(moniker_options()).unwrap();
        assert_eq!(
            moniker.set_moniker(MonikerOptions {
                work_done_progress_options: progress(Some(false)),
            }),
            Err(BuildError::ConflictingCapability {
                field: "monikerProvider"
            })
        );
    }
}
