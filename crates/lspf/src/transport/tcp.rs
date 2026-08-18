//! The single-connection TCP adapter: `Content-Length` framing over one
//! accepted socket (ADR 0011).

use std::net::SocketAddr;

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio_util::codec::FramedRead;

use super::framing::ContentLengthCodec;
use super::{
    Transport, TransportError, TransportReader, TransportWriter, classify_io_error, envelope,
};
use crate::builder::Server;
use crate::raw::RawMessage;

/// Entry point: serve a built [`Server`] to exactly one native TCP client.
///
/// The builder binds `addr` once, reports the bound address through
/// [`TcpBuilder::on_bound`] (so port 0 is usable), accepts the first
/// connection, and serves it until it ends — a second connection is never
/// accepted. The adapter supplies the transport and nothing else, and serving
/// reports how the connection ended rather than terminating the process (ADR
/// 0018); multi-client serving and TLS are deliberately not introduced (ADR
/// 0011).
///
/// ```no_run
/// # struct State;
/// # async fn run() -> lspf::Result<()> {
/// let server = lspf::Server::builder(State)
///     .build()
///     .expect("static registrations are valid");
/// let outcome = lspf::tcp(server, "127.0.0.1:9257").serve().await?;
/// std::process::exit(outcome.code());
/// # Ok(())
/// # }
/// ```
pub fn tcp<S, A>(server: Server<S>, addr: A) -> TcpBuilder<S, A>
where
    S: Send + Sync + 'static,
    A: ToSocketAddrs,
{
    TcpBuilder {
        server,
        addr,
        max_message_size: None,
        on_bound: None,
    }
}

pub struct TcpBuilder<S, A> {
    server: Server<S>,
    addr: A,
    max_message_size: Option<usize>,
    on_bound: Option<Box<dyn FnOnce(SocketAddr) + Send>>,
}

impl<S, A> TcpBuilder<S, A> {
    /// Cap the `Content-Length` body the adapter accepts or sends. The
    /// default is 16 MiB, shared with stdio.
    pub fn max_message_size(mut self, limit: usize) -> Self {
        self.max_message_size = Some(limit);
        self
    }

    /// Observe the address the listener actually bound, before the first
    /// connection is accepted. With a port-0 `addr` this is how the chosen
    /// port becomes known.
    pub fn on_bound(mut self, hook: impl FnOnce(SocketAddr) + Send + 'static) -> Self {
        self.on_bound = Some(Box::new(hook));
        self
    }
}

impl<S, A> TcpBuilder<S, A>
where
    S: Send + Sync + 'static,
    A: ToSocketAddrs,
{
    /// Bind, accept the first connection, and serve it until it ends,
    /// reporting the [`Outcome`](crate::Outcome) that ended it.
    ///
    /// Equivalent to [`Server::serve`] over [`TcpTransport`]; the process is
    /// never terminated here. Bind, local-address, and accept failures are
    /// returned as [`Error::Transport`](crate::Error::Transport) with their
    /// I/O source retained.
    pub async fn serve(self) -> crate::Result<crate::Outcome> {
        let listener = TcpListener::bind(self.addr)
            .await
            .map_err(TransportError::Io)?;
        let local_addr = listener.local_addr().map_err(TransportError::Io)?;
        if let Some(on_bound) = self.on_bound {
            on_bound(local_addr);
        }
        let (stream, _peer) = listener.accept().await.map_err(TransportError::Io)?;
        // One server serves one connection (ADR 0011): the listener is gone
        // before the session starts, so no second client is ever accepted.
        drop(listener);
        let transport = match self.max_message_size {
            Some(limit) => TcpTransport::from_stream_with_limit(stream, limit),
            None => TcpTransport::from_stream(stream),
        }
        .map_err(TransportError::Io)?;
        self.server.serve(transport).await
    }
}

/// The TCP [`Transport`]: one accepted socket carrying the same
/// `Content-Length` wire contract as stdio (ADR 0011).
pub struct TcpTransport {
    reader: TcpReader,
    writer: TcpWriter,
}

pub struct TcpReader {
    framed_in: FramedRead<OwnedReadHalf, ContentLengthCodec>,
}

pub struct TcpWriter {
    sink: OwnedWriteHalf,
    codec: ContentLengthCodec,
}

impl TcpTransport {
    /// Serve an already accepted stream with the default 16 MiB body limit.
    ///
    /// Enables TCP_NODELAY: LSP messages are small and latency-bound, so the
    /// adapter never waits to coalesce them.
    pub fn from_stream(stream: TcpStream) -> std::io::Result<Self> {
        Self::from_stream_with_limit(stream, super::framing::DEFAULT_MAX_SIZE)
    }

    /// [`from_stream`](Self::from_stream) with a caller-chosen body limit;
    /// the builder-facing knob is [`TcpBuilder::max_message_size`].
    pub(crate) fn from_stream_with_limit(
        stream: TcpStream,
        max_message_size: usize,
    ) -> std::io::Result<Self> {
        stream.set_nodelay(true)?;
        let (read, write) = stream.into_split();
        Ok(Self {
            reader: TcpReader {
                framed_in: FramedRead::new(read, ContentLengthCodec::new(max_message_size)),
            },
            writer: TcpWriter {
                sink: write,
                codec: ContentLengthCodec::new(max_message_size),
            },
        })
    }
}

impl Transport for TcpTransport {
    type Reader = TcpReader;
    type Writer = TcpWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (self.reader, self.writer)
    }
}

impl TransportReader for TcpReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        let body = self
            .framed_in
            .next()
            .await
            .ok_or(TransportError::Closed)??;
        Ok(envelope::parse(body))
    }
}

impl TransportWriter for TcpWriter {
    async fn send(&mut self, msg: RawMessage) -> Result<(), TransportError> {
        let body = envelope::serialize(&msg)?;
        let header = self.codec.header_for(body.len())?;
        self.sink
            .write_all(header.as_bytes())
            .await
            .map_err(classify_io_error)?;
        self.sink
            .write_all(&body)
            .await
            .map_err(classify_io_error)?;
        self.sink.flush().await.map_err(classify_io_error)?;
        Ok(())
    }

    async fn shutdown(mut self) -> Result<(), TransportError> {
        self.sink.flush().await.map_err(classify_io_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use serde_json::{Value, json};
    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::Outcome;
    use crate::transport::conformance::{self, ContentLengthClient, WireClient};

    impl ContentLengthClient<OwnedReadHalf, OwnedWriteHalf> {
        async fn connect(addr: SocketAddr) -> Self {
            let stream = TcpStream::connect(addr)
                .await
                .expect("the test client connects");
            let (reader, writer) = stream.into_split();
            Self::new(reader, writer)
        }
    }

    fn initialize() -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "processId": null, "rootUri": null, "capabilities": {} },
        })
    }

    /// Bind a listener and serve its first accepted connection, the embedding
    /// path behind `TcpTransport::from_stream`.
    async fn serve_first(
        listener: TcpListener,
    ) -> (SocketAddr, impl Future<Output = crate::Result<Outcome>>) {
        let addr = listener.local_addr().expect("the listener has an address");
        let serving = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("the client connects");
            let transport = TcpTransport::from_stream(stream).expect("TCP_NODELAY is set");
            conformance::server().serve(transport).await
        });
        let serving = async move { serving.await.expect("the serving task does not panic") };
        (addr, serving)
    }

    /// Serve `lspf::tcp(server, "127.0.0.1:0")` on a task, announcing the
    /// bound address through a oneshot.
    fn serve_on_ephemeral_port(
        max_message_size: Option<usize>,
    ) -> (
        futures_channel::oneshot::Receiver<SocketAddr>,
        impl Future<Output = crate::Result<Outcome>>,
    ) {
        let (tx, rx) = futures_channel::oneshot::channel();
        let mut builder = crate::tcp(conformance::server(), "127.0.0.1:0").on_bound(move |addr| {
            let _ = tx.send(addr);
        });
        if let Some(limit) = max_message_size {
            builder = builder.max_message_size(limit);
        }
        let serving = tokio::spawn(builder.serve());
        let serving = async move { serving.await.expect("the serving task does not panic") };
        (rx, serving)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tcp_passes_the_shared_transport_conformance_journey() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the test listener binds");
        let (addr, serving) = serve_first(listener).await;
        let mut client = ContentLengthClient::connect(addr).await;

        conformance::run(&mut client, serving).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tcp_reports_the_bound_address_for_port_zero() {
        let (rx, serving) = serve_on_ephemeral_port(None);
        let addr = rx.await.expect("the bound address is reported");
        assert_ne!(addr.port(), 0, "port 0 resolves to a real port");

        let mut client = ContentLengthClient::connect(addr).await;
        client.send(initialize()).await;
        let initialized = client.receive().await;
        assert_eq!(initialized["id"], 1);
        drop(client);

        let outcome = serving
            .await
            .expect("a disconnecting client is a normal transport ending");
        assert_eq!(outcome, Outcome::TransportClosed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tcp_serves_only_the_first_connection() {
        let (rx, serving) = serve_on_ephemeral_port(None);
        let addr = rx.await.expect("the bound address is reported");

        let mut first = ContentLengthClient::connect(addr).await;
        first.send(initialize()).await;
        assert_eq!(first.receive().await["id"], 1);

        // The listener is dropped once the first connection is accepted, so a
        // second client is never served: either the connect itself is
        // refused, or the connected socket is reset or never answered.
        match TcpStream::connect(addr).await {
            Err(error) => assert_eq!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused,
                "the dropped listener refuses new connections"
            ),
            Ok(stream) => {
                let (reader, writer) = stream.into_split();
                let mut second = ContentLengthClient::new(reader, writer);
                second.send(initialize()).await;
                let answered = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    second.reader.next(),
                )
                .await;
                assert!(
                    !matches!(answered, Ok(Some(Ok(_)))),
                    "the second connection must never receive a response, got {answered:?}"
                );
            }
        }

        first
            .send(json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }))
            .await;
        assert_eq!(first.receive().await["id"], 2);
        first
            .send(json!({ "jsonrpc": "2.0", "method": "exit" }))
            .await;
        let outcome = serving.await.expect("the first connection exits cleanly");
        assert_eq!(outcome, Outcome::Exit { code: 0 });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tcp_enforces_the_configured_message_size_limit() {
        let (rx, serving) = serve_on_ephemeral_port(Some(64));
        let addr = rx.await.expect("the bound address is reported");

        let mut client = ContentLengthClient::connect(addr).await;
        client
            .send(json!({
                "jsonrpc": "2.0",
                "method": "conformance/oversized",
                "params": "this body is well over the configured sixty-four byte limit",
            }))
            .await;

        let error = serving
            .await
            .expect_err("an oversized inbound body fails the serving");
        assert!(matches!(
            error,
            crate::Error::Transport(TransportError::OversizedMessage { limit: 64, .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tcp_from_stream_defaults_to_a_sixteen_mib_limit() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the test listener binds");
        let (addr, serving) = serve_first(listener).await;

        let mut client = TcpStream::connect(addr)
            .await
            .expect("the test client connects");
        client
            .write_all(format!("Content-Length: {}\r\n\r\n", 16 * 1024 * 1024 + 1).as_bytes())
            .await
            .expect("the header writes");

        let error = serving
            .await
            .expect_err("an oversized inbound header fails the serving");
        assert!(matches!(
            error,
            crate::Error::Transport(TransportError::OversizedMessage {
                limit: 16_777_216,
                ..
            })
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tcp_peer_half_close_uses_the_common_close_path() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the test listener binds");
        let (addr, serving) = serve_first(listener).await;

        let mut client = ContentLengthClient::connect(addr).await;
        client.send(initialize()).await;
        assert_eq!(client.receive().await["id"], 1);
        // FIN the write side while keeping the read side open: the peer is
        // gone for reading, which is the engine's ordinary transport close.
        client
            .writer
            .shutdown()
            .await
            .expect("the client half-closes");

        let outcome = serving
            .await
            .expect("a peer half-close is a normal transport ending");
        assert_eq!(outcome, Outcome::TransportClosed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tcp_bind_errors_keep_their_io_source() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the test listener binds");
        let addr = listener.local_addr().expect("the listener has an address");

        let error = crate::tcp(conformance::server(), addr)
            .serve()
            .await
            .expect_err("binding a taken address fails");
        match error {
            crate::Error::Transport(TransportError::Io(io)) => {
                assert_eq!(io.kind(), std::io::ErrorKind::AddrInUse);
            }
            other => panic!("expected the bind failure's io source, got {other:?}"),
        }
    }
}
