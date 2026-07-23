mod envelope;
pub mod framing;
#[cfg(not(target_arch = "wasm32"))]
mod stdio;

use std::future::Future;
use std::io;

use thiserror::Error;

use crate::raw::RawMessage;
use crate::server::LanguageServer;

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
/// Entry point: wrap a `LanguageServer` in the default stdio adapter.
///
/// ```no_run
/// # async fn run() -> lspf::Result<()> {
/// # struct Hello { documents: lspf::Documents }
/// # impl lspf::LanguageServer for Hello {
/// #     fn documents(&self) -> &lspf::Documents { &self.documents }
/// # }
/// lspf::stdio(Hello { documents: lspf::Documents::new() }).serve().await
/// # }
/// ```
pub fn stdio<S: LanguageServer>(server: S) -> StdioBuilder<S> {
    StdioBuilder {
        server,
        concurrency_limit: crate::DEFAULT_CONCURRENCY_LIMIT,
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct StdioBuilder<S> {
    server: S,
    concurrency_limit: usize,
}

#[cfg(not(target_arch = "wasm32"))]
impl<S: LanguageServer> StdioBuilder<S> {
    /// Override the default cap on in-flight handler tasks (ADR 0012,
    /// default [`crate::DEFAULT_CONCURRENCY_LIMIT`]).
    pub fn concurrency_limit(mut self, limit: usize) -> Self {
        self.concurrency_limit = limit;
        self
    }

    pub async fn serve(self) -> crate::Result<()> {
        let transport = StdioTransport::new();
        match crate::dispatcher::run(self.server, transport, self.concurrency_limit).await? {
            // Peer hung up before `exit`: return normally and let the
            // caller's `main` decide the process disposition.
            crate::dispatcher::Outcome::TransportClosed => Ok(()),
            // `exit` notification: terminate the process with the LSP
            // exit code, per the spec's lifecycle contract.
            crate::dispatcher::Outcome::Exit(code) => std::process::exit(code),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;

    use super::{
        RawMessage, Transport, TransportError, TransportReader, TransportWriter, envelope,
    };
    use crate::{Documents, LanguageServer, RequestId};

    struct FrameTransport {
        frames: VecDeque<Bytes>,
        outbox: Arc<Mutex<Vec<RawMessage>>>,
    }

    struct FrameReader {
        frames: VecDeque<Bytes>,
    }

    struct FrameWriter {
        outbox: Arc<Mutex<Vec<RawMessage>>>,
    }

    impl Transport for FrameTransport {
        type Reader = FrameReader;
        type Writer = FrameWriter;

        fn split(self) -> (Self::Reader, Self::Writer) {
            (
                FrameReader {
                    frames: self.frames,
                },
                FrameWriter {
                    outbox: self.outbox,
                },
            )
        }
    }

    impl TransportReader for FrameReader {
        async fn recv(&mut self) -> Result<RawMessage, TransportError> {
            self.frames
                .pop_front()
                .map(envelope::parse)
                .ok_or(TransportError::Closed)
        }
    }

    impl TransportWriter for FrameWriter {
        async fn send(&mut self, msg: RawMessage) -> Result<(), TransportError> {
            self.outbox.lock().unwrap().push(msg);
            Ok(())
        }

        async fn shutdown(self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    struct TestServer {
        documents: Documents,
    }

    impl LanguageServer for TestServer {
        fn documents(&self) -> &Documents {
            &self.documents
        }
    }

    #[tokio::test]
    async fn complete_protocol_error_frames_do_not_close_the_connection() {
        let outbox = Arc::new(Mutex::new(Vec::new()));
        let transport = FrameTransport {
            frames: VecDeque::from([
                Bytes::from_static(br#"{"jsonrpc":"2.0","method":"initialize""#),
                Bytes::from_static(br#"{"jsonrpc":"1.0","method":"initialize"}"#),
                Bytes::from_static(
                    br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#,
                ),
            ]),
            outbox: outbox.clone(),
        };

        crate::serve(
            TestServer {
                documents: Documents::new(),
            },
            transport,
        )
        .await
        .expect("complete protocol errors do not become transport errors");

        let outbox = outbox.lock().unwrap();
        let error_codes: Vec<_> = outbox
            .iter()
            .filter_map(|message| match message {
                RawMessage::ProtocolError { error } => Some(error.code),
                _ => None,
            })
            .collect();
        assert_eq!(error_codes, vec![-32700, -32600]);
        assert!(
            outbox.iter().any(|message| {
                matches!(message, RawMessage::Response { id, result: Ok(_) } if *id == RequestId::Number(1))
            }),
            "initialize after protocol errors should still be processed, got outbox {outbox:#?}"
        );
    }
}
