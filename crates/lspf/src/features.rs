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
    CodeActionRequest, CodeActionResolveRequest, CodeLensRequest, CodeLensResolve, Completion,
    DocumentDiagnosticRequest, DocumentLinkRequest, DocumentLinkResolve, HoverRequest,
    InlayHintRequest, InlayHintResolveRequest, PrepareRenameRequest, Rename, Request,
    ResolveCompletionItem, WillCreateFiles, WillDeleteFiles, WillRenameFiles, WillSaveWaitUntil,
    WorkspaceDiagnosticRequest, WorkspaceSymbolRequest, WorkspaceSymbolResolve,
};
use lsp_types::{
    CodeActionOptions, CodeLensOptions, CompletionOptions, DiagnosticOptions, DocumentLinkOptions,
    FileOperationRegistrationOptions, InlayHintOptions, RenameOptions, WorkspaceSymbolOptions,
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
    fn contribute(&self, _caps: &mut CapabilityBuilder) -> Result<(), BuildError> {
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
        CodeActionRequest, CodeActionResolveRequest, CodeLensRequest, CodeLensResolve,
        DocumentDiagnosticRequest, DocumentLinkRequest, DocumentLinkResolve, InlayHintRequest,
        InlayHintResolveRequest, PrepareRenameRequest, Rename, WorkspaceDiagnosticRequest,
    };
    use lsp_types::{
        CodeAction, CodeActionOptions, CodeActionParams, CodeActionResponse, CodeLens,
        CodeLensOptions, CodeLensParams, CreateFilesParams, DeleteFilesParams, DiagnosticOptions,
        DidChangeWatchedFilesParams, DocumentDiagnosticParams, DocumentDiagnosticReportResult,
        DocumentLink, DocumentLinkOptions, DocumentLinkParams, InlayHint, InlayHintOptions,
        InlayHintParams, PrepareRenameResponse, RenameFilesParams, RenameOptions, RenameParams,
        TextDocumentPositionParams, WorkspaceDiagnosticParams, WorkspaceDiagnosticReportResult,
        WorkspaceEdit, WorkspaceSymbol, WorkspaceSymbolParams, WorkspaceSymbolResponse,
    };

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
