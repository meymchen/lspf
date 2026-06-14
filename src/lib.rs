//! lspf — a Rust framework for building extensible LSP language servers.
//!
//! See `CONTEXT.md` and `docs/adr/` at the repository root for the domain
//! language and the architectural decisions that shape this crate.

mod context;
mod dispatcher;
mod error;
mod raw;
mod server;
mod transport;

pub mod types {
    //! LSP protocol types — re-exported from `lsp-types` per ADR 0014.
    pub use lsp_types::*;
}

pub use context::Context;
pub use error::{Error, LspError, Result};
pub use raw::{JsonRpcError, RawMessage, RequestId};
pub use server::LanguageServer;
pub use transport::{
    StdioBuilder, StdioReader, StdioTransport, StdioWriter, Transport, TransportError,
    TransportReader, TransportWriter, stdio,
};

/// Cancellation primitive passed to every request handler (ADR 0007).
pub use tokio_util::sync::CancellationToken;

/// Default cap on in-flight handler tasks (ADR 0012).
pub const DEFAULT_CONCURRENCY_LIMIT: usize = 64;

/// Drive a `LanguageServer` over a custom `Transport`.
///
/// `stdio()` is the canonical entry point; `serve()` is the escape hatch
/// for tests and for transports beyond stdio (TCP, WebSocket, in-process
/// mocks). See ADR 0011 for the transport contract. Uses
/// [`DEFAULT_CONCURRENCY_LIMIT`] for in-flight handlers; use
/// [`serve_with_limit`] to override.
pub async fn serve<S, T>(server: S, transport: T) -> Result<()>
where
    S: LanguageServer,
    T: Transport,
{
    dispatcher::run(server, transport, DEFAULT_CONCURRENCY_LIMIT).await
}

/// Like [`serve`], but with an explicit cap on in-flight handler tasks
/// (ADR 0012). When the cap is hit, the read-loop awaits a permit before
/// spawning the next handler — visible in traces as a long
/// `handler.acquire_permit` span.
pub async fn serve_with_limit<S, T>(server: S, transport: T, concurrency_limit: usize) -> Result<()>
where
    S: LanguageServer,
    T: Transport,
{
    dispatcher::run(server, transport, concurrency_limit).await
}
