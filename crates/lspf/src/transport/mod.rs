// Envelope helpers (ADR 0011) exist wherever a Transport adapter exists —
// every adapter parses and serializes the same JSON-RPC envelopes.
#[cfg(all(test, feature = "runtime-tokio", not(target_arch = "wasm32")))]
mod conformance_support {
    pub(crate) use crate::{LspError, Outcome, Result, Server, ServerContext, TaskSend};

    #[cfg(all(not(target_arch = "wasm32"), any(feature = "stdio", feature = "tcp")))]
    pub(crate) use crate::transport::framing::ContentLengthCodec;
}
#[cfg(all(test, feature = "runtime-tokio", not(target_arch = "wasm32")))]
mod conformance;
// Outbound accounting always uses `envelope::serialize`; without a Transport
// feature the inbound parser is intentionally dormant.
#[cfg_attr(
    not(any(
        test,
        feature = "stdio",
        feature = "tcp",
        feature = "websocket",
        feature = "worker-channel"
    )),
    allow(dead_code)
)]
pub(crate) mod envelope;
// `Content-Length` framing is the wire contract of stdio and TCP only.
#[cfg(any(feature = "stdio", feature = "tcp"))]
pub mod framing;
#[cfg(all(feature = "stdio", not(target_arch = "wasm32")))]
mod stdio;
#[cfg(all(feature = "tcp", not(target_arch = "wasm32")))]
mod tcp;
#[cfg(all(feature = "websocket", not(target_arch = "wasm32")))]
mod websocket;
#[cfg(all(feature = "worker-channel", target_arch = "wasm32"))]
mod worker_channel;

use std::future::Future;
use std::io;

use thiserror::Error;

#[cfg(all(feature = "stdio", not(target_arch = "wasm32")))]
use crate::builder::Server;
use crate::raw::RawMessage;

#[cfg(all(feature = "stdio", not(target_arch = "wasm32")))]
pub use stdio::{StdioReader, StdioTransport, StdioWriter};
#[cfg(all(feature = "tcp", not(target_arch = "wasm32")))]
pub use tcp::{TcpBuilder, TcpReader, TcpTransport, TcpWriter, tcp};
#[cfg(all(feature = "websocket", not(target_arch = "wasm32")))]
pub use websocket::{
    WebSocketBuilder, WebSocketReader, WebSocketTransport, WebSocketWriter, websocket,
};
#[cfg(all(feature = "worker-channel", target_arch = "wasm32"))]
pub use worker_channel::{
    WorkerChannelBuilder, WorkerChannelReader, WorkerChannelTransport, WorkerChannelWriter,
    worker_channel,
};

/// I/O failures that mean the peer is gone collapse to
/// [`TransportError::Closed`]; every other I/O failure keeps its source.
#[cfg(all(
    any(feature = "stdio", feature = "tcp", feature = "websocket"),
    not(target_arch = "wasm32")
))]
pub(crate) fn classify_io_error(error: io::Error) -> TransportError {
    match error.kind() {
        io::ErrorKind::BrokenPipe
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::NotConnected
        | io::ErrorKind::UnexpectedEof => TransportError::Closed,
        _ => TransportError::Io(error),
    }
}

/// A Transport framing, channel, or serialization failure.
#[derive(Debug, Error)]
pub enum TransportError {
    /// An underlying I/O operation failed.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// The peer or local channel closed normally.
    #[error("connection closed by peer")]
    Closed,

    /// An incoming message was not a valid supported JSON-RPC envelope.
    #[error("malformed message: {0}")]
    Malformed(String),

    /// An incoming or outgoing message exceeded its configured size limit.
    #[error("message exceeds size limit ({length} > {limit} bytes)")]
    OversizedMessage {
        /// Actual message size in bytes.
        length: usize,
        /// Configured maximum size in bytes.
        limit: usize,
    },

    /// An outgoing message could not be serialized.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// A message-framed channel for LSP JSON-RPC envelopes (see ADR 0011).
///
/// Concrete implementations split into a [`TransportReader`] and a
/// [`TransportWriter`] so the protocol engine's read-loop and send-loop
/// can own the two halves independently (ADR 0015). Framing
/// (`Content-Length` for stdio/TCP, none for the message-framed
/// transports) is the adapter's concern, never the engine's.
///
/// The `Send` supertrait applies to native targets only: on
/// `wasm32-unknown-unknown` a transport may hold thread-affine JavaScript
/// values across awaits (ADR 0020), and the framework never fakes `Send`.
#[cfg(not(target_arch = "wasm32"))]
pub trait Transport: Send + 'static {
    /// Read half produced by [`Transport::split`].
    type Reader: TransportReader;
    /// Write half produced by [`Transport::split`].
    type Writer: TransportWriter;

    /// Split the channel into independently owned read and write halves.
    fn split(self) -> (Self::Reader, Self::Writer);
}

/// A message-framed channel for LSP JSON-RPC envelopes on WASM.
///
/// This is the thread-affine counterpart of the native [`Transport`] trait;
/// it deliberately has no `Send` bound (ADR 0020).
#[cfg(target_arch = "wasm32")]
pub trait Transport: 'static {
    /// Read half produced by [`Transport::split`].
    type Reader: TransportReader;
    /// Write half produced by [`Transport::split`].
    type Writer: TransportWriter;

    /// Split the channel into independently owned read and write halves.
    fn split(self) -> (Self::Reader, Self::Writer);
}

/// Read half of a [`Transport`] (ADR 0011, ADR 0015).
#[cfg(not(target_arch = "wasm32"))]
pub trait TransportReader: Send + 'static {
    /// Receive the next decoded envelope.
    fn recv(
        &mut self,
    ) -> impl Future<Output = std::result::Result<RawMessage, TransportError>> + Send;
}

/// Read half of a WASM [`Transport`] (ADR 0011, ADR 0015).
#[cfg(target_arch = "wasm32")]
pub trait TransportReader: 'static {
    /// Receive the next decoded envelope.
    fn recv(&mut self) -> impl Future<Output = std::result::Result<RawMessage, TransportError>>;
}

/// Write half of a [`Transport`] (ADR 0011, ADR 0015). `shutdown`
/// consumes the writer so the send-loop task can flush remaining bytes
/// after the outgoing channel is drained.
#[cfg(not(target_arch = "wasm32"))]
pub trait TransportWriter: Send + 'static {
    /// Serialize and send one envelope.
    fn send(
        &mut self,
        msg: RawMessage,
    ) -> impl Future<Output = std::result::Result<(), TransportError>> + Send;

    /// Consume the writer and flush any buffered output.
    fn shutdown(self) -> impl Future<Output = std::result::Result<(), TransportError>> + Send;
}

/// Write half of a WASM [`Transport`] (ADR 0011, ADR 0015).
#[cfg(target_arch = "wasm32")]
pub trait TransportWriter: 'static {
    /// Serialize and send one envelope.
    fn send(
        &mut self,
        msg: RawMessage,
    ) -> impl Future<Output = std::result::Result<(), TransportError>>;

    /// Consume the writer and flush any buffered output.
    fn shutdown(self) -> impl Future<Output = std::result::Result<(), TransportError>>;
}

#[cfg(all(feature = "stdio", not(target_arch = "wasm32")))]
/// Entry point: serve a built [`Server`] over the default stdio adapter.
///
/// The adapter supplies the transport and nothing else. Concurrency policy,
/// registrations, and lifecycle hooks all belong to the [`Server`] that was
/// handed in, and serving reports how the connection ended rather than
/// terminating the process — mapping an [`Outcome`](crate::Outcome) to a
/// process disposition is the binary's decision (ADR 0018).
///
/// ```no_run
/// # struct State;
/// # async fn run() -> lspf::Result<()> {
/// let server = lspf::Server::builder(State)
///     .build()
///     .expect("static registrations are valid");
/// let outcome = lspf::stdio(server).serve().await?;
/// std::process::exit(outcome.code());
/// # }
/// ```
pub fn stdio<S>(server: Server<S>) -> StdioBuilder<S>
where
    S: Send + Sync + 'static,
{
    StdioBuilder { server }
}

#[cfg(all(feature = "stdio", not(target_arch = "wasm32")))]
/// A default-stdio Server builder ready to be served.
pub struct StdioBuilder<S> {
    server: Server<S>,
}

#[cfg(all(feature = "stdio", not(target_arch = "wasm32")))]
impl<S> StdioBuilder<S>
where
    S: Send + Sync + 'static,
{
    /// Serve the connection over stdio until it ends, and report the
    /// [`Outcome`](crate::Outcome) that ended it.
    ///
    /// Equivalent to [`Server::serve`] over [`StdioTransport`]; the process is
    /// never terminated here.
    pub async fn serve(self) -> crate::Result<crate::Outcome> {
        self.server.serve(StdioTransport::new()).await
    }
}

#[cfg(all(
    test,
    not(target_arch = "wasm32"),
    any(
        feature = "stdio",
        feature = "tcp",
        feature = "websocket",
        feature = "worker-channel"
    )
))]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;

    use super::{
        RawMessage, Transport, TransportError, TransportReader, TransportWriter, envelope,
    };
    use crate::{Outcome, RequestId, Server};

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

    struct TestState;

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

        let server = Server::builder(TestState)
            .build()
            .expect("an empty server builds");
        let outcome = server
            .serve(transport)
            .await
            .expect("complete protocol errors do not become transport errors");
        assert_eq!(
            outcome,
            Outcome::TransportClosed,
            "the frames run out, so the connection ends on reader EOF"
        );

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
