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
    CompletionOptions, DiagnosticOptions, DiagnosticServerCapabilities, ExecuteCommandOptions,
    FileOperationRegistrationOptions, HoverProviderCapability, OneOf, ServerCapabilities,
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
    file_create: FileOperationFamily,
    file_rename: FileOperationFamily,
    file_delete: FileOperationFamily,
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
    /// The completion and workspace-symbol families each fold their resolve
    /// flag into the base options as `resolveProvider`, emitting one merged
    /// provider capability per family. The diagnostics family emits its shared
    /// options as one `diagnosticProvider`. Each file-operation family emits
    /// its shared filters under exactly the `will*`/`did*` sides that
    /// registered, merged into one `workspace.fileOperations` object. The
    /// execute-command field appears only when at least one command was
    /// registered, and its command list exactly matches the frozen registry:
    /// de-duplicated and in registration order (ADR 0022).
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
}
