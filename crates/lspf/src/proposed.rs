//! Compatibility aliases for refresh requests that were formerly proposed.
//!
//! The generated LSP 3.18 type base now supplies these request markers and
//! parameters through [`crate::types`]. These feature-gated aliases retain
//! lspf's former proposed-module names for compatibility.

pub use gen_lsp_types::{
    FoldingRangeRefreshRequest as FoldingRangeRefresh, TextDocumentContentRefreshParams,
    TextDocumentContentRefreshRequest as TextDocumentContentRefresh,
};
