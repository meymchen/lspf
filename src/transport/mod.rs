mod envelope;
pub mod framing;
mod stdio;

use std::future::Future;
use std::io;

use thiserror::Error;

use crate::raw::RawMessage;
use crate::server::LanguageServer;

pub use stdio::{StdioReader, StdioTransport, StdioWriter};

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
/// Concrete implementations split into a [`TransportReader`] and a
/// [`TransportWriter`] so the dispatcher's read-loop and send-loop can
/// own the two halves independently (ADR 0015). Framing
/// (`Content-Length` for stdio/TCP, none for the message-framed
/// transports) is the adapter's concern, never the dispatcher's.
pub trait Transport: Send + 'static {
    type Reader: TransportReader;
    type Writer: TransportWriter;

    fn split(self) -> (Self::Reader, Self::Writer);
}

/// Read half of a [`Transport`] (ADR 0011, ADR 0015).
pub trait TransportReader: Send + 'static {
    fn recv(
        &mut self,
    ) -> impl Future<Output = std::result::Result<RawMessage, TransportError>> + Send;
}

/// Write half of a [`Transport`] (ADR 0011, ADR 0015). `shutdown`
/// consumes the writer so the send-loop task can flush remaining bytes
/// after the outgoing channel is drained.
pub trait TransportWriter: Send + 'static {
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
    StdioBuilder {
        server,
        concurrency_limit: crate::DEFAULT_CONCURRENCY_LIMIT,
    }
}

pub struct StdioBuilder<S> {
    server: S,
    concurrency_limit: usize,
}

impl<S: LanguageServer> StdioBuilder<S> {
    /// Override the default cap on in-flight handler tasks (ADR 0012,
    /// default [`crate::DEFAULT_CONCURRENCY_LIMIT`]).
    pub fn concurrency_limit(mut self, limit: usize) -> Self {
        self.concurrency_limit = limit;
        self
    }

    pub async fn serve(self) -> crate::Result<()> {
        let transport = StdioTransport::new();
        crate::dispatcher::run(self.server, transport, self.concurrency_limit).await
    }
}
