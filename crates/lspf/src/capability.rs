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

use lsp_types::{
    CodeActionOptions, CodeActionProviderCapability, CodeLensOptions, CompletionOptions,
    DiagnosticOptions, DiagnosticServerCapabilities, DocumentLinkOptions, ExecuteCommandOptions,
    FileOperationRegistrationOptions, HoverProviderCapability, InlayHintOptions,
    InlayHintServerCapabilities, OneOf, RenameOptions, ServerCapabilities,
    WorkspaceFileOperationsServerCapabilities, WorkspaceServerCapabilities, WorkspaceSymbolOptions,
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
    /// Contribute the hover capability. Hover carries no options, so repeated
    /// contributions are identical and never conflict; the caller already
    /// rejects a duplicate `textDocument/hover` handler before reaching here.
    pub(crate) fn set_hover(&mut self) -> Result<(), BuildError> {
        self.caps.hover_provider = Some(HoverProviderCapability::Simple(true));
        Ok(())
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
    pub(crate) fn finish(mut self) -> ServerCapabilities {
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
            self.caps.workspace_symbol_provider = Some(OneOf::Right(options));
        }
        if let Some(mut options) = self.rename.options {
            if self.rename.prepare {
                options.prepare_provider = Some(true);
            }
            self.caps.rename_provider = Some(OneOf::Right(options));
        }
        if let Some(mut options) = self.code_actions.options {
            if self.code_actions.resolve {
                options.resolve_provider = Some(true);
            }
            self.caps.code_action_provider = Some(CodeActionProviderCapability::Options(options));
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
            self.caps.inlay_hint_provider =
                Some(OneOf::Right(InlayHintServerCapabilities::Options(options)));
        }
        if !self.commands.is_empty() {
            self.caps.execute_command_provider = Some(ExecuteCommandOptions {
                commands: self.commands,
                work_done_progress_options: Default::default(),
            });
        }
        if let Some(options) = self.diagnostics.options {
            self.caps.diagnostic_provider = Some(DiagnosticServerCapabilities::Options(options));
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
            let file_operations = WorkspaceFileOperationsServerCapabilities {
                did_create: self.file_create.did_options(),
                will_create: self.file_create.will_options(),
                did_rename: self.file_rename.did_options(),
                will_rename: self.file_rename.will_options(),
                did_delete: self.file_delete.did_options(),
                will_delete: self.file_delete.will_options(),
            };
            self.caps.workspace = Some(WorkspaceServerCapabilities {
                file_operations: Some(file_operations),
                ..WorkspaceServerCapabilities::default()
            });
        }
        self.caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::CodeActionKind;

    #[test]
    fn hover_sets_only_hover_provider() {
        let mut caps = CapabilityBuilder::default();
        caps.set_hover().unwrap();
        let caps = caps.finish();
        assert_eq!(
            caps.hover_provider,
            Some(HoverProviderCapability::Simple(true))
        );
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
            Some(DiagnosticServerCapabilities::Options(options))
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
        caps.set_workspace_symbol(options.clone()).unwrap();
        assert_eq!(
            caps.finish().workspace_symbol_provider,
            Some(OneOf::Right(options))
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
            OneOf::Right(workspace_symbol_options(Some(true))),
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
                work_done_progress_options: lsp_types::WorkDoneProgressOptions {
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
            filters: vec![lsp_types::FileOperationFilter {
                scheme: Some("file".to_string()),
                pattern: lsp_types::FileOperationPattern {
                    glob: glob.to_string(),
                    matches: Some(lsp_types::FileOperationPatternKind::File),
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
        caps.set_rename(options.clone()).unwrap();
        assert_eq!(caps.finish().rename_provider, Some(OneOf::Right(options)));
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
            OneOf::Right(rename_options(Some(true))),
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
            code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
            work_done_progress_options: Default::default(),
            resolve_provider,
        }
    }

    #[test]
    fn code_action_advertises_supplied_options() {
        let options = code_action_options(None);
        let mut caps = CapabilityBuilder::default();
        caps.set_code_action(options.clone()).unwrap();
        assert_eq!(
            caps.finish().code_action_provider,
            Some(CodeActionProviderCapability::Options(options))
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
            CodeActionProviderCapability::Options(code_action_options(Some(true))),
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
        CodeLensOptions { resolve_provider }
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
        caps.set_document_link(options.clone()).unwrap();
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
                work_done_progress_options: lsp_types::WorkDoneProgressOptions {
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
        caps.set_inlay_hint(options.clone()).unwrap();
        assert_eq!(
            caps.finish().inlay_hint_provider,
            Some(OneOf::Right(InlayHintServerCapabilities::Options(options)))
        );
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
            OneOf::Right(InlayHintServerCapabilities::Options(inlay_hint_options(
                Some(true)
            ))),
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
                work_done_progress_options: lsp_types::WorkDoneProgressOptions {
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
}
