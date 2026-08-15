//! lspf — a Rust framework for building extensible LSP language servers.
//!
//! See `CONTEXT.md` and `docs/adr/` at the repository root for the domain
//! language and the architectural decisions that shape this crate.

mod builder;
mod capability;
mod client;
mod codec;
mod context;
mod documents;
mod engine;
mod error;
pub mod features;
mod file_provider;
mod progress;
#[cfg(feature = "proposed")]
pub mod proposed;
mod raw;
mod runtime;
mod service;
mod transport;
mod uri_key;
mod workspace;

#[cfg(test)]
mod test_util;

pub mod types {
    //! LSP protocol types — re-exported from `lsp-types` per ADR 0014.
    pub use lsp_types::*;
}

/// Compiles the Rust in the repository's user-facing Markdown as doc-tests, so
/// a quickstart or a guide example cannot drift from the surface it documents.
/// The module exists only under `cargo test --doc`; nothing here is part of
/// the public API.
#[cfg(doctest)]
mod markdown {
    #[doc = include_str!("../../../README.md")]
    pub struct Readme;

    #[doc = include_str!("../../../README.zh-CN.md")]
    pub struct ReadmeZhCn;

    #[doc = include_str!("../../../docs/guides/features-and-workspace.md")]
    pub struct FeaturesAndWorkspaceGuide;
}

pub use builder::{InitializeRegistrar, Server, ServerBuilder};
pub use client::{Client, TelemetryEventParams};
pub use context::Context;
pub use documents::{Document, DocumentsView, PositionEncoding};
pub use engine::Outcome;
pub use error::{BuildError, ClientError, Error, LspError, ProgressError, Result};
pub use features::{FeatureSpec, NotificationFeatureSpec};
pub use file_provider::{FileProvider, MemoryFileProvider};
#[cfg(not(target_arch = "wasm32"))]
pub use file_provider::{OsFileProvider, OsFileProviderBuilder};
pub use progress::{ProgressHandle, ProgressOptions};
pub use raw::{JsonRpcError, RawMessage, RequestId};
#[doc(hidden)]
pub use runtime::TaskSend;
pub use service::{CallKind, IncomingCall, Layer, Next, ServiceFuture, ServiceResult};
#[cfg(not(target_arch = "wasm32"))]
pub use transport::{StdioBuilder, StdioReader, StdioTransport, StdioWriter, stdio};
pub use transport::{Transport, TransportError, TransportReader, TransportWriter};
pub use workspace::{Workspace, WorkspaceError};

/// Cancellation primitive passed to every request handler (ADR 0007).
pub use tokio_util::sync::CancellationToken;

/// Cap on calls in flight inside the user Layer chain when a [`Server`] does
/// not set its own with [`ServerBuilder::concurrency_limit`] (ADR 0012).
///
/// The cap is connection policy, so it belongs to the built [`Server`] that
/// owns the connection rather than to the `Transport` it is served over.
pub const DEFAULT_CONCURRENCY_LIMIT: usize = 64;
