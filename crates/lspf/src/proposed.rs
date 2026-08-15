//! Proposed LSP protocol surface, available only with the crate's `proposed`
//! Cargo feature.
//!
//! `lsp-types` 0.97.x ships only the stable protocol, so the proposed
//! workspace refresh requests get their request markers and parameter types
//! here. Everything in this module — and the matching `Client` helpers — is
//! compiled out of default builds: enabling the feature only adds API, it
//! never changes the stable Router catalog or the capabilities a server
//! advertises.

use lsp_types::Uri;
use lsp_types::request::Request;
use serde::{Deserialize, Serialize};

/// The proposed `workspace/foldingRange/refresh` request marker.
///
/// The request asks the client to recompute its folding ranges. It carries no
/// parameters (sent as `null`) and the client acknowledges with a `null`
/// result, decoded as `()`.
pub enum FoldingRangeRefresh {}

impl Request for FoldingRangeRefresh {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "workspace/foldingRange/refresh";
}

/// The parameters of the proposed `workspace/textDocumentContent/refresh`
/// request.
///
/// The params name exactly one target: the document whose content the client
/// should re-pull. The spec types that field as a `DocumentUri`; `lsp-types`
/// 0.97.x models document URIs with its [`Uri`] type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextDocumentContentRefreshParams {
    /// The document the client should refresh.
    pub uri: Uri,
}

/// The proposed `workspace/textDocumentContent/refresh` request marker.
///
/// The request asks the client to refresh the cached content of one document;
/// the client acknowledges with a `null` result, decoded as `()`.
pub enum TextDocumentContentRefresh {}

impl Request for TextDocumentContentRefresh {
    type Params = TextDocumentContentRefreshParams;
    type Result = ();
    const METHOD: &'static str = "workspace/textDocumentContent/refresh";
}
