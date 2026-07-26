//! The internal capability catalog (ADR 0017).
//!
//! Standard features and commands each contribute to one destination field of
//! [`ServerCapabilities`]. [`CapabilityBuilder`] accumulates those
//! contributions by field rather than by method, so a family spread across
//! several methods — today only execute-command, whose commands merge into one
//! de-duplicated list — produces a single deterministic capability regardless
//! of registration order. Custom requests and notifications contribute nothing.
//!
//! Merging never uses last-write-wins: a second contribution that disagrees
//! with an already-recorded singular field is a [`BuildError::ConflictingCapability`],
//! surfaced by [`ServerBuilder::build`](crate::ServerBuilder::build).

use std::collections::BTreeSet;

use lsp_types::{
    CompletionOptions, ExecuteCommandOptions, HoverProviderCapability, ServerCapabilities,
};

use crate::error::BuildError;

/// Accumulates standard-feature and command capability contributions and
/// freezes them into one [`ServerCapabilities`] value.
///
/// The `BTreeSet` of command names makes the execute-command list both
/// de-duplicated and order-independent; the remaining fields land directly on
/// the in-progress `ServerCapabilities`. Protocol-owned negotiated fields (for
/// example ADR 0016's position encoding) are layered on separately by the
/// engine and never pass through here.
#[derive(Default)]
pub(crate) struct CapabilityBuilder {
    caps: ServerCapabilities,
    commands: BTreeSet<String>,
}

impl CapabilityBuilder {
    /// Contribute the hover capability. Hover carries no options, so repeated
    /// contributions are identical and never conflict; the caller already
    /// rejects a duplicate `textDocument/hover` handler before reaching here.
    pub(crate) fn set_hover(&mut self) -> Result<(), BuildError> {
        self.caps.hover_provider = Some(HoverProviderCapability::Simple(true));
        Ok(())
    }

    /// Contribute the supplied completion options. Two completion features that
    /// advertise different options cannot both be honored, so a mismatch is a
    /// [`BuildError::ConflictingCapability`] rather than a silent overwrite.
    pub(crate) fn set_completion(&mut self, options: CompletionOptions) -> Result<(), BuildError> {
        match &self.caps.completion_provider {
            Some(existing) if *existing != options => Err(BuildError::ConflictingCapability {
                field: "completionProvider",
            }),
            _ => {
                self.caps.completion_provider = Some(options);
                Ok(())
            }
        }
    }

    /// Add one command name to the execute-command capability. Duplicate names
    /// are rejected earlier as a [`BuildError::DuplicateCommand`]; the set only
    /// guarantees the emitted list is sorted and de-duplicated.
    pub(crate) fn add_command(&mut self, name: String) {
        self.commands.insert(name);
    }

    /// Freeze the accumulated contributions into a `ServerCapabilities`.
    ///
    /// The execute-command field appears only when at least one command was
    /// registered, and its command list is deterministic (sorted) regardless
    /// of the order the commands were declared in.
    pub(crate) fn finish(mut self) -> ServerCapabilities {
        if !self.commands.is_empty() {
            self.caps.execute_command_provider = Some(ExecuteCommandOptions {
                commands: self.commands.into_iter().collect(),
                work_done_progress_options: Default::default(),
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

    #[test]
    fn commands_merge_into_one_sorted_deduplicated_list() {
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
            vec!["a.cmd".to_string(), "b.cmd".to_string()],
            "commands are sorted and de-duplicated, so the list is order-independent"
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
