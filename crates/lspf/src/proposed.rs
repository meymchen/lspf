//! **Unstable proposed API.** Proposed LSP protocol surface, available only
//! with the crate's `proposed` Cargo feature.
//!
//! The generated LSP 3.18 type base now supplies these request markers and
//! parameters directly. The aliases retain lspf's existing proposed-module
//! names while the feature remains an opt-in compatibility boundary.

pub use gen_lsp_types::{
    FoldingRangeRefreshRequest as FoldingRangeRefresh, TextDocumentContentRefreshParams,
    TextDocumentContentRefreshRequest as TextDocumentContentRefresh,
};
