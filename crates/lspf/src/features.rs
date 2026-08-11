//! Standard LSP feature descriptors (ADR 0017).
//!
//! Each `features::*` constructor returns a descriptor implementing the sealed
//! [`FeatureSpec`] trait. A descriptor fixes three things at once: the
//! `lsp_types` request marker that names the wire method and its typed
//! parameters and result, the descriptor's public options, and the single
//! deterministic contribution the feature makes to the capability catalog.
//!
//! [`FeatureSpec`] is sealed: downstream crates use these descriptors but
//! cannot implement the trait, so they cannot present a pseudo-standard feature
//! whose capability fragment lspf does not know how to merge. Custom methods
//! use [`request`](crate::ServerBuilder::request) and
//! [`notification`](crate::ServerBuilder::notification) instead and advertise
//! no capability.

use lsp_types::request::{
    Completion, DocumentDiagnosticRequest, HoverRequest, Request, ResolveCompletionItem,
    WorkspaceDiagnosticRequest,
};
use lsp_types::{CompletionOptions, DiagnosticOptions};

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
        caps.set_document_diagnostics(self.options.clone())
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
        caps.set_workspace_diagnostics(self.options.clone())
    }
}
impl FeatureSpec for WorkspaceDiagnosticFeature {
    type Marker = WorkspaceDiagnosticRequest;
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::request::{DocumentDiagnosticRequest, WorkspaceDiagnosticRequest};
    use lsp_types::{
        DiagnosticOptions, DocumentDiagnosticParams, DocumentDiagnosticReportResult,
        WorkspaceDiagnosticParams, WorkspaceDiagnosticReportResult,
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
}
