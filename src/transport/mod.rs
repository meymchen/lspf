mod envelope;
pub mod framing;
mod stdio;

use std::future::Future;
use std::io;

use thiserror::Error;

use crate::raw::RawMessage;
use crate::server::LanguageServer;

pub use stdio::StdioTransport;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    #[error("connection closed by peer")]
    Closed,

    #[error("malformed message: {0}")]
    Malformed(String),

    #[error("message exceeds size limit ({length} > {limit} bytes)")]
    OversizedMessage { length: usize, limit: usize },

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// A message-framed channel for LSP JSON-RPC envelopes (see ADR 0011).
///
/// One call to `recv` yields one envelope; one call to `send` writes one
/// envelope. Framing (`Content-Length` for stdio/TCP, none for the
/// message-framed transports) is the adapter's concern, never the
/// dispatcher's.
pub trait Transport: Send + 'static {
    fn recv(
        &mut self,
    ) -> impl Future<Output = std::result::Result<RawMessage, TransportError>> + Send;

    fn send(
        &mut self,
        msg: RawMessage,
    ) -> impl Future<Output = std::result::Result<(), TransportError>> + Send;

    fn shutdown(self) -> impl Future<Output = std::result::Result<(), TransportError>> + Send;
}

/// Entry point: wrap a `LanguageServer` in the default stdio adapter.
///
/// ```no_run
/// # async fn run() -> lspf::Result<()> {
/// # struct Hello;
/// # impl lspf::LanguageServer for Hello {}
/// lspf::stdio(Hello).serve().await
/// # }
/// ```
pub fn stdio<S: LanguageServer>(server: S) -> StdioBuilder<S> {
    StdioBuilder { server }
}

pub struct StdioBuilder<S> {
    server: S,
}

impl<S: LanguageServer> StdioBuilder<S> {
    pub async fn serve(self) -> crate::Result<()> {
        let transport = StdioTransport::new();
        crate::dispatcher::run(self.server, transport).await
    }
}
