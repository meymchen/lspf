//! Language features, one module per LSP capability family.

pub(crate) mod code_actions;
pub(crate) mod completion;
pub(crate) mod definition;
pub(crate) mod diagnostics;
pub(crate) mod file_rename;
pub(crate) mod folding;
pub(crate) mod hover;
pub(crate) mod links;
pub(crate) mod references;
pub(crate) mod rename;
pub(crate) mod selection;
pub(crate) mod symbols;
