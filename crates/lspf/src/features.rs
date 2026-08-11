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
    Completion, DocumentDiagnosticRequest, HoverRequest, Request, ResolveCompletionItem,
    WillCreateFiles, WillDeleteFiles, WillRenameFiles, WorkspaceDiagnosticRequest,
    WorkspaceSymbolRequest, WorkspaceSymbolResolve,
};
use lsp_types::{
    CompletionOptions, DiagnosticOptions, FileOperationRegistrationOptions, WorkspaceSymbolOptions,
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
    use lsp_types::request::{DocumentDiagnosticRequest, WorkspaceDiagnosticRequest};
    use lsp_types::{
        CreateFilesParams, DeleteFilesParams, DiagnosticOptions, DidChangeWatchedFilesParams,
        DocumentDiagnosticParams, DocumentDiagnosticReportResult, RenameFilesParams,
        WorkspaceDiagnosticParams, WorkspaceDiagnosticReportResult, WorkspaceEdit, WorkspaceSymbol,
        WorkspaceSymbolParams, WorkspaceSymbolResponse,
    };

    fn assert_document_descriptor<F: FeatureSpec<Marker = DocumentDiagnosticRequest>>(_: F) {}
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
            <WillCreateFiles as Request>::METHOD,
            <WillRenameFiles as Request>::METHOD,
            <WillDeleteFiles as Request>::METHOD,
        ] {
            assert_ne!(method, "workspace/executeCommand");
        }
    }
}
