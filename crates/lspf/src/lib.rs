//! lspf — a Rust framework for building extensible LSP language servers.
//!
//! See `CONTEXT.md` and `docs/adr/` at the repository root for the domain
//! language and the architectural decisions that shape this crate.

mod builder;
mod capability;
mod client;
mod codec;
mod context;
mod dispatcher;
mod documents;
mod engine;
mod error;
pub mod features;
mod raw;
mod runtime;
mod server;
mod service;
mod transport;
mod workspace;

pub mod types {
    //! LSP protocol types — re-exported from `lsp-types` per ADR 0014.
    pub use lsp_types::*;
}

/// Compiles the Rust in the repository's user-facing Markdown as doc-tests, so
/// a quickstart or a migration example cannot drift from the surface it
/// documents. The module exists only under `cargo test --doc`; nothing here is
/// part of the public API.
#[cfg(doctest)]
mod markdown {
    #[doc = include_str!("../../../README.md")]
    pub struct Readme;

    #[doc = include_str!("../../../README.zh-CN.md")]
    pub struct ReadmeZhCn;

    #[doc = include_str!("../../../docs/migrations/0.1-to-0.2.md")]
    pub struct MigrationGuide;
}

pub use builder::{InitializeRegistrar, Server, ServerBuilder};
pub use client::Client;
pub use context::Context;
pub use documents::{Document, Documents, DocumentsView, PositionEncoding};
pub use engine::Outcome;
pub use error::{BuildError, ClientError, Error, LspError, Result};
pub use features::FeatureSpec;
pub use raw::{JsonRpcError, RawMessage, RequestId};
pub use server::LanguageServer;
pub use service::{CallKind, IncomingCall, Layer, Next, ServiceFuture, ServiceResult};
#[cfg(not(target_arch = "wasm32"))]
pub use transport::{StdioBuilder, StdioReader, StdioTransport, StdioWriter, stdio};
pub use transport::{Transport, TransportError, TransportReader, TransportWriter};
pub use workspace::Workspace;

/// Cancellation primitive passed to every request handler (ADR 0007).
pub use tokio_util::sync::CancellationToken;

/// Default cap on in-flight handler tasks (ADR 0012).
pub const DEFAULT_CONCURRENCY_LIMIT: usize = 64;

/// Drive a 0.1 `LanguageServer` over a custom `Transport`.
///
/// The 0.2 entry points are [`stdio`] for the default adapter and
/// [`Server::serve`] for any other `Transport` — a built [`Server`] carries its
/// own concurrency policy and reports an [`Outcome`] instead of terminating the
/// process. This function and [`serve_with_limit`] remain only for the
/// superseded trait-based core and are removed with it. See ADR 0011 for the
/// transport contract. Uses [`DEFAULT_CONCURRENCY_LIMIT`] for in-flight
/// handlers; use [`serve_with_limit`] to override.
pub async fn serve<S, T>(server: S, transport: T) -> Result<()>
where
    S: LanguageServer,
    T: Transport,
{
    dispatcher::run(server, transport, DEFAULT_CONCURRENCY_LIMIT).await?;
    Ok(())
}

/// Like [`serve`], but with an explicit cap on in-flight handler tasks
/// (ADR 0012). When the cap is hit, the read-loop awaits a permit before
/// spawning the next handler — visible in traces as a long
/// `handler.acquire_permit` span. A 0.2 [`Server`] sets the same cap through
/// [`ServerBuilder::concurrency_limit`] instead.
pub async fn serve_with_limit<S, T>(server: S, transport: T, concurrency_limit: usize) -> Result<()>
where
    S: LanguageServer,
    T: Transport,
{
    dispatcher::run(server, transport, concurrency_limit).await?;
    Ok(())
}
