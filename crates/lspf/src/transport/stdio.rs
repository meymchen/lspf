use std::pin::Pin;

use futures_util::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio_util::codec::FramedRead;

use super::framing::ContentLengthCodec;
use super::{
    Transport, TransportError, TransportReader, TransportWriter, classify_io_error, envelope,
};
use crate::raw::RawMessage;

/// A `Content-Length` framed Transport over process standard input and output.
pub struct StdioTransport {
    reader: StdioReader,
    writer: StdioWriter,
}

/// Read half of [`StdioTransport`].
pub struct StdioReader {
    framed_in: FramedRead<Pin<Box<dyn AsyncRead + Send>>, ContentLengthCodec>,
}

/// Write half of [`StdioTransport`].
pub struct StdioWriter {
    stdout: Pin<Box<dyn AsyncWrite + Send>>,
    codec: ContentLengthCodec,
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl StdioTransport {
    /// Connect a Transport to the process standard input and output streams.
    pub fn new() -> Self {
        Self {
            reader: StdioReader {
                framed_in: FramedRead::new(
                    Box::pin(tokio::io::stdin()),
                    ContentLengthCodec::default(),
                ),
            },
            writer: StdioWriter {
                stdout: Box::pin(tokio::io::stdout()),
                codec: ContentLengthCodec::default(),
            },
        }
    }

    #[cfg(test)]
    fn from_io<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + 'static,
        W: AsyncWrite + Send + 'static,
    {
        Self {
            reader: StdioReader {
                framed_in: FramedRead::new(Box::pin(reader), ContentLengthCodec::default()),
            },
            writer: StdioWriter {
                stdout: Box::pin(writer),
                codec: ContentLengthCodec::default(),
            },
        }
    }
}

impl Transport for StdioTransport {
    type Reader = StdioReader;
    type Writer = StdioWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (self.reader, self.writer)
    }
}

impl TransportReader for StdioReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        let body = self
            .framed_in
            .next()
            .await
            .ok_or(TransportError::Closed)??;
        Ok(envelope::parse(body))
    }
}

impl TransportWriter for StdioWriter {
    async fn send(&mut self, msg: RawMessage) -> Result<(), TransportError> {
        let body = envelope::serialize(&msg)?;
        let header = self.codec.header_for(body.len())?;
        self.stdout
            .write_all(header.as_bytes())
            .await
            .map_err(classify_io_error)?;
        self.stdout
            .write_all(&body)
            .await
            .map_err(classify_io_error)?;
        self.stdout.flush().await.map_err(classify_io_error)?;
        Ok(())
    }

    async fn shutdown(mut self) -> Result<(), TransportError> {
        self.stdout.flush().await.map_err(classify_io_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use bytes::Bytes;

    use super::*;
    use crate::transport::conformance::{self, ContentLengthClient};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stdio_passes_the_shared_transport_conformance_journey() {
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server_io);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let transport = StdioTransport::from_io(server_reader, server_writer);
        let serving = tokio::spawn(conformance::server().serve(transport));
        let serving = async move { serving.await.expect("the serving task does not panic") };
        let mut client = ContentLengthClient::new(client_reader, client_writer);

        conformance::run(&mut client, serving).await;
    }

    #[tokio::test]
    async fn stdio_rejects_oversized_output_before_writing() {
        let mut writer = StdioWriter {
            stdout: Box::pin(tokio::io::sink()),
            codec: ContentLengthCodec::default(),
        };
        let message = RawMessage::Notification {
            method: Cow::Borrowed("conformance/oversized"),
            params: Bytes::from(vec![b'x'; 16 * 1024 * 1024]),
        };

        let error = writer.send(message).await.unwrap_err();
        assert!(matches!(
            error,
            TransportError::OversizedMessage {
                limit: 16_777_216,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn stdio_send_after_peer_close_returns_closed() {
        let (server_io, peer_io) = tokio::io::duplex(1024);
        drop(peer_io);
        let mut writer = StdioWriter {
            stdout: Box::pin(server_io),
            codec: ContentLengthCodec::default(),
        };
        let message = RawMessage::Notification {
            method: Cow::Borrowed("conformance/closed"),
            params: Bytes::new(),
        };

        let error = writer.send(message).await.unwrap_err();
        assert!(matches!(error, TransportError::Closed));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stdio_eof_uses_the_common_close_path() {
        let (server_io, peer_io) = tokio::io::duplex(1024);
        let (server_reader, server_writer) = tokio::io::split(server_io);
        let transport = StdioTransport::from_io(server_reader, server_writer);
        drop(peer_io);

        let outcome = conformance::server()
            .serve(transport)
            .await
            .expect("EOF is a normal transport ending");
        assert_eq!(outcome, crate::Outcome::TransportClosed);
    }
}
