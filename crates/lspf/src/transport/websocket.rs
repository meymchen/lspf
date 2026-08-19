//! The single-connection native WebSocket adapter (ADR 0011).

use std::net::SocketAddr;

use bytes::Bytes;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::value::RawValue;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, ToSocketAddrs};
use tokio_tungstenite::tungstenite::error::CapacityError;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use tokio_tungstenite::{WebSocketStream, accept_async_with_config};

use super::{
    Transport, TransportError, TransportReader, TransportWriter, classify_io_error, envelope,
};
use crate::builder::Server;
use crate::raw::RawMessage;

const DEFAULT_MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Entry point: serve a built [`Server`] to exactly one native WebSocket client.
///
/// The builder binds `addr` once, completes the standard server handshake for
/// the first accepted connection, and serves that connection until it ends.
/// WebSocket control frames and message reassembly stay inside the adapter;
/// the protocol engine receives one UTF-8 JSON envelope per data message.
///
/// ```no_run
/// # struct State;
/// # async fn run() -> lspf::Result<()> {
/// let server = lspf::Server::builder(State)
///     .build()
///     .expect("static registrations are valid");
/// let outcome = lspf::websocket(server, "127.0.0.1:9258").serve().await?;
/// std::process::exit(outcome.code());
/// # }
/// ```
pub fn websocket<S, A>(server: Server<S>, addr: A) -> WebSocketBuilder<S, A>
where
    S: Send + Sync + 'static,
    A: ToSocketAddrs,
{
    WebSocketBuilder {
        server,
        addr,
        max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        on_bound: None,
    }
}

/// Configures one native WebSocket listener before serving its first client.
pub struct WebSocketBuilder<S, A> {
    server: Server<S>,
    addr: A,
    max_message_size: usize,
    on_bound: Option<Box<dyn FnOnce(SocketAddr) + Send>>,
}

impl<S, A> WebSocketBuilder<S, A> {
    /// Cap each complete reassembled incoming or outgoing WebSocket message.
    /// The default is 16 MiB.
    pub fn max_message_size(mut self, limit: usize) -> Self {
        self.max_message_size = limit;
        self
    }

    /// Observe the address the listener actually bound before accepting.
    pub fn on_bound(mut self, hook: impl FnOnce(SocketAddr) + Send + 'static) -> Self {
        self.on_bound = Some(Box::new(hook));
        self
    }
}

impl<S, A> WebSocketBuilder<S, A>
where
    S: Send + Sync + 'static,
    A: ToSocketAddrs,
{
    /// Bind once, complete the server handshake for the first connection, and
    /// serve it until the common protocol-engine close path completes.
    pub async fn serve(self) -> crate::Result<crate::Outcome> {
        let listener = TcpListener::bind(self.addr)
            .await
            .map_err(TransportError::Io)?;
        let local_addr = listener.local_addr().map_err(TransportError::Io)?;
        if let Some(on_bound) = self.on_bound {
            on_bound(local_addr);
        }
        let (stream, _peer) = listener.accept().await.map_err(TransportError::Io)?;
        drop(listener);

        let config = WebSocketConfig::default()
            .max_message_size(Some(self.max_message_size))
            .max_frame_size(Some(self.max_message_size));
        let stream = accept_async_with_config(stream, Some(config))
            .await
            .map_err(classify_error)?;
        self.server
            .serve(WebSocketTransport::from_stream_with_limit(
                stream,
                self.max_message_size,
            ))
            .await
    }
}

/// A transport over an already-established WebSocket stream.
pub struct WebSocketTransport<S> {
    reader: WebSocketReader<S>,
    writer: WebSocketWriter<S>,
}

/// Read half of a [`WebSocketTransport`].
pub struct WebSocketReader<S> {
    stream: SplitStream<WebSocketStream<S>>,
    max_message_size: usize,
}

/// Write half of a [`WebSocketTransport`].
pub struct WebSocketWriter<S> {
    sink: SplitSink<WebSocketStream<S>, Message>,
    max_message_size: usize,
}

impl<S> WebSocketTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Wrap an established WebSocket stream with the default 16 MiB complete
    /// message limit.
    pub fn from_stream(stream: WebSocketStream<S>) -> Self {
        Self::from_stream_with_limit(stream, DEFAULT_MAX_MESSAGE_SIZE)
    }

    fn from_stream_with_limit(stream: WebSocketStream<S>, max_message_size: usize) -> Self {
        let (sink, stream) = stream.split();
        Self {
            reader: WebSocketReader {
                stream,
                max_message_size,
            },
            writer: WebSocketWriter {
                sink,
                max_message_size,
            },
        }
    }
}

impl<S> Transport for WebSocketTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Reader = WebSocketReader<S>;
    type Writer = WebSocketWriter<S>;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (self.reader, self.writer)
    }
}

impl<S> TransportReader for WebSocketReader<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        loop {
            let message = self
                .stream
                .next()
                .await
                .ok_or(TransportError::Closed)?
                .map_err(classify_error)?;
            match message {
                Message::Text(text) => {
                    return parse_message(
                        Bytes::copy_from_slice(text.as_bytes()),
                        self.max_message_size,
                    );
                }
                Message::Binary(data) => {
                    std::str::from_utf8(&data).map_err(|error| {
                        TransportError::Malformed(format!("binary message is not UTF-8: {error}"))
                    })?;
                    return parse_message(data, self.max_message_size);
                }
                Message::Close(_) => return Err(TransportError::Closed),
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Frame(_) => {
                    return Err(TransportError::Malformed(
                        "unexpected raw WebSocket frame".to_string(),
                    ));
                }
            }
        }
    }
}

impl<S> TransportWriter for WebSocketWriter<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    async fn send(&mut self, msg: RawMessage) -> Result<(), TransportError> {
        let body = envelope::serialize(&msg)?;
        enforce_limit(body.len(), self.max_message_size)?;
        let text = String::from_utf8(body)
            .map_err(|error| TransportError::Malformed(error.to_string()))?;
        self.sink
            .send(Message::text(text))
            .await
            .map_err(classify_error)
    }

    async fn shutdown(mut self) -> Result<(), TransportError> {
        // Reading a peer close queues Tungstenite's close reply. Flush that
        // control frame before attempting our own close; on the peer-close
        // path the subsequent send reports `SendAfterClosing`, which is a
        // successful shutdown because the queued reply is already on the wire.
        self.sink.flush().await.map_err(classify_error)?;
        match self.sink.send(Message::Close(None)).await {
            Ok(())
            | Err(WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed)
            | Err(WebSocketError::Protocol(ProtocolError::SendAfterClosing)) => Ok(()),
            Err(error) => Err(classify_error(error)),
        }
    }
}

fn parse_message(body: Bytes, max_message_size: usize) -> Result<RawMessage, TransportError> {
    enforce_limit(body.len(), max_message_size)?;
    serde_json::from_slice::<&RawValue>(&body)
        .map_err(|error| TransportError::Malformed(format!("invalid JSON envelope: {error}")))?;
    Ok(envelope::parse(body))
}

fn enforce_limit(length: usize, limit: usize) -> Result<(), TransportError> {
    if length > limit {
        Err(TransportError::OversizedMessage { length, limit })
    } else {
        Ok(())
    }
}

fn classify_error(error: WebSocketError) -> TransportError {
    match error {
        WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed => TransportError::Closed,
        WebSocketError::Io(error) => classify_io_error(error),
        WebSocketError::Capacity(CapacityError::MessageTooLong { size, max_size }) => {
            TransportError::OversizedMessage {
                length: size,
                limit: max_size,
            }
        }
        other => TransportError::Malformed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::Value;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::protocol::frame::Frame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::{Data, OpCode};
    use tokio_tungstenite::{WebSocketStream, accept_async, client_async};

    use super::WebSocketTransport;
    use crate::transport::conformance::{self, WireClient};
    use crate::{Outcome, RawMessage, Transport, TransportError, TransportReader};

    struct WebSocketClient(WebSocketStream<TcpStream>);

    async fn connected_pair() -> (WebSocketStream<TcpStream>, WebSocketStream<TcpStream>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the test listener binds");
        let addr = listener.local_addr().expect("the listener has an address");
        let server = async {
            let (stream, _) = listener.accept().await.expect("the client connects");
            accept_async(stream)
                .await
                .expect("the server completes the handshake")
        };
        let client = async {
            let stream = TcpStream::connect(addr)
                .await
                .expect("the test client connects");
            client_async("ws://localhost", stream)
                .await
                .expect("the client completes the handshake")
                .0
        };
        tokio::join!(server, client)
    }

    impl WireClient for WebSocketClient {
        async fn send(&mut self, message: Value) {
            self.0
                .send(Message::text(message.to_string()))
                .await
                .expect("send test WebSocket message");
        }

        async fn receive(&mut self) -> Value {
            let message = self
                .0
                .next()
                .await
                .expect("the server writes a message")
                .expect("the server message is valid WebSocket");
            serde_json::from_slice(&message.into_data()).expect("the server message contains JSON")
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn established_websocket_passes_the_shared_transport_conformance_journey() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the test listener binds");
        let addr = listener.local_addr().expect("the listener has an address");
        let serving = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("the client connects");
            let websocket = accept_async(stream)
                .await
                .expect("the server completes the handshake");
            conformance::server()
                .serve(WebSocketTransport::from_stream(websocket))
                .await
        });
        let stream = TcpStream::connect(addr)
            .await
            .expect("the test client connects");
        let (websocket, _) = client_async("ws://localhost", stream)
            .await
            .expect("the client completes the handshake");
        let mut client = WebSocketClient(websocket);
        let serving = async move { serving.await.expect("the serving task does not panic") };

        conformance::run(&mut client, serving).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn binary_data_message_is_one_utf8_json_envelope() {
        let (server, mut client) = connected_pair().await;
        let (mut reader, _writer) = WebSocketTransport::from_stream(server).split();
        client
            .send(Message::Binary(Bytes::from_static(
                br#"{"jsonrpc":"2.0","method":"binary/example","params":{"text":"h\u00e9llo"}}"#,
            )))
            .await
            .expect("send the binary message");

        let message = reader.recv().await.expect("receive the binary envelope");
        assert!(matches!(
            message,
            RawMessage::Notification { method, .. } if method == "binary/example"
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn malformed_data_messages_are_distinct_from_io_errors() {
        let cases = [
            (Message::Binary(Bytes::from_static(b"\xff")), "not UTF-8"),
            (
                Message::text(
                    r#"{"jsonrpc":"2.0","method":"one"}{"jsonrpc":"2.0","method":"two"}"#,
                ),
                "invalid JSON envelope",
            ),
            (Message::text("{not json}"), "invalid JSON envelope"),
            (
                Message::text("Content-Length: 2\r\n\r\n{}"),
                "invalid JSON envelope",
            ),
        ];

        for (message, expected) in cases {
            let (server, mut client) = connected_pair().await;
            let (mut reader, _writer) = WebSocketTransport::from_stream(server).split();
            client.send(message).await.expect("send malformed message");

            let error = reader.recv().await.expect_err("the message is malformed");
            assert!(
                matches!(&error, TransportError::Malformed(detail) if detail.contains(expected)),
                "expected malformed detail containing {expected:?}, got {error:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fragmented_message_is_reassembled_while_ping_stays_a_control_frame() {
        let (server, mut client) = connected_pair().await;
        let (mut reader, _writer) = WebSocketTransport::from_stream(server).split();
        client
            .send(Message::Frame(Frame::message(
                Bytes::from_static(br#"{"jsonrpc":"2.0","method":"fragment/"#),
                OpCode::Data(Data::Text),
                false,
            )))
            .await
            .expect("send the first fragment");
        client
            .send(Message::Ping(Bytes::from_static(b"still-here")))
            .await
            .expect("send ping between fragments");
        client
            .send(Message::Frame(Frame::message(
                Bytes::from_static(br#"example","params":{}}"#),
                OpCode::Data(Data::Continue),
                true,
            )))
            .await
            .expect("send the final fragment");

        let message = reader.recv().await.expect("receive reassembled envelope");
        assert!(matches!(
            message,
            RawMessage::Notification { method, .. } if method == "fragment/example"
        ));
        let pong = client
            .next()
            .await
            .expect("the server answers the ping")
            .expect("the pong is valid WebSocket");
        assert_eq!(pong, Message::Pong(Bytes::from_static(b"still-here")));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_frame_enters_the_common_protocol_engine_close_path() {
        let (tx, rx) = futures_channel::oneshot::channel();
        let serving = tokio::spawn(
            crate::websocket(conformance::server(), "127.0.0.1:0")
                .on_bound(move |addr| {
                    let _ = tx.send(addr);
                })
                .serve(),
        );
        let addr = rx.await.expect("the bound address is reported");
        let stream = TcpStream::connect(addr)
            .await
            .expect("the test client connects");
        let (mut client, _) = client_async("ws://localhost", stream)
            .await
            .expect("the client completes the handshake");
        client
            .send(Message::Close(None))
            .await
            .expect("send close frame");

        let reply = client
            .next()
            .await
            .expect("the server replies to the close")
            .expect("the close reply is valid WebSocket");
        assert!(matches!(reply, Message::Close(_)));

        let outcome = serving
            .await
            .expect("the serving task does not panic")
            .expect("a close frame is a normal transport ending");
        assert_eq!(outcome, Outcome::TransportClosed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn default_limit_applies_to_the_complete_reassembled_message() {
        let (server, mut client) = connected_pair().await;
        let (mut reader, _writer) = WebSocketTransport::from_stream(server).split();
        let fragment_size = 8 * 1024 * 1024 + 1;
        let sending = tokio::spawn(async move {
            client
                .send(Message::Frame(Frame::message(
                    vec![b' '; fragment_size],
                    OpCode::Data(Data::Binary),
                    false,
                )))
                .await
                .expect("send the first large fragment");
            client
                .send(Message::Frame(Frame::message(
                    vec![b' '; fragment_size],
                    OpCode::Data(Data::Continue),
                    true,
                )))
                .await
                .expect("send the final large fragment");
        });

        let error = reader
            .recv()
            .await
            .expect_err("the reassembled message exceeds the default limit");
        assert!(matches!(
            error,
            TransportError::OversizedMessage {
                length: 16_777_218,
                limit: 16_777_216,
            }
        ));
        sending.await.expect("the sending task does not panic");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_builder_serves_only_the_first_connection() {
        let (tx, rx) = futures_channel::oneshot::channel();
        let serving = tokio::spawn(
            crate::websocket(conformance::server(), "127.0.0.1:0")
                .on_bound(move |addr| {
                    let _ = tx.send(addr);
                })
                .serve(),
        );
        let addr = rx.await.expect("the bound address is reported");
        let stream = TcpStream::connect(addr)
            .await
            .expect("the first client connects");
        let (first, _) = client_async("ws://localhost", stream)
            .await
            .expect("the first client completes the handshake");
        let mut first = WebSocketClient(first);
        first
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "processId": null, "rootUri": null, "capabilities": {} },
            }))
            .await;
        assert_eq!(first.receive().await["id"], 1);

        match TcpStream::connect(addr).await {
            Err(error) => assert_eq!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused,
                "the dropped listener refuses new connections"
            ),
            Ok(stream) => {
                let second = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    client_async("ws://localhost", stream),
                )
                .await;
                assert!(
                    !matches!(second, Ok(Ok(_))),
                    "the second connection must not complete a handshake"
                );
            }
        }

        first
            .send(serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }))
            .await;
        assert_eq!(first.receive().await["id"], 2);
        first
            .send(serde_json::json!({ "jsonrpc": "2.0", "method": "exit" }))
            .await;
        let outcome = serving
            .await
            .expect("the serving task does not panic")
            .expect("the first connection exits cleanly");
        assert_eq!(outcome, Outcome::Exit { code: 0 });
    }
}
