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
    HoverProviderCapability, ServerCapabilities,
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
    diagnostics: Option<DiagnosticOptions>,
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
        match &self.diagnostics {
            Some(existing) if *existing != options => Err(BuildError::ConflictingCapability {
                field: "diagnosticProvider",
            }),
            _ => {
                self.diagnostics = Some(options);
                Ok(())
            }
        }
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
            None if self.completion.resolve => Err(clash()),
            // The resolve feature contributes `resolveProvider = true`; a base
            // that explicitly denies it cannot be honored by last-write-wins.
            Some(options) if self.completion.resolve && options.resolve_provider == Some(false) => {
                Err(clash())
            }
            _ => Ok(()),
        }
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
    /// The completion family folds its resolve flag into the base options as
    /// `resolveProvider`, emitting one merged `completionProvider`. The
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
        if !self.commands.is_empty() {
            self.caps.execute_command_provider = Some(ExecuteCommandOptions {
                commands: self.commands,
                work_done_progress_options: Default::default(),
            });
        }
        if let Some(options) = self.diagnostics {
            self.caps.diagnostic_provider = Some(DiagnosticServerCapabilities::Options(options));
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
    fn compatible_diagnostic_routes_merge_independent_of_order() {
        let options = diagnostic_options(Some("compiler"), true);
        let mut document_first = CapabilityBuilder::default();
        document_first.set_diagnostics(options.clone()).unwrap();
        document_first.set_diagnostics(options.clone()).unwrap();

        let mut workspace_first = CapabilityBuilder::default();
        workspace_first.set_diagnostics(options.clone()).unwrap();
        workspace_first.set_diagnostics(options.clone()).unwrap();

        let document_first = document_first.finish().diagnostic_provider;
        let workspace_first = workspace_first.finish().diagnostic_provider;
        assert_eq!(document_first, workspace_first);
        assert_eq!(
            document_first,
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
}
