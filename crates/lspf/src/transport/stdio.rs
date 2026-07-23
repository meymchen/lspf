use futures_util::StreamExt;
use tokio::io::{AsyncWriteExt, Stdin, Stdout};
use tokio_util::codec::FramedRead;

use super::framing::ContentLengthCodec;
use super::{Transport, TransportError, TransportReader, TransportWriter, envelope};
use crate::raw::RawMessage;

pub struct StdioTransport {
    reader: StdioReader,
    writer: StdioWriter,
}

pub struct StdioReader {
    framed_in: FramedRead<Stdin, ContentLengthCodec>,
}

pub struct StdioWriter {
    stdout: Stdout,
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl StdioTransport {
    pub fn new() -> Self {
        Self {
            reader: StdioReader {
                framed_in: FramedRead::new(tokio::io::stdin(), ContentLengthCodec::default()),
            },
            writer: StdioWriter {
                stdout: tokio::io::stdout(),
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
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdout.write_all(header.as_bytes()).await?;
        self.stdout.write_all(&body).await?;
        self.stdout.flush().await?;
        Ok(())
    }

    async fn shutdown(mut self) -> Result<(), TransportError> {
        self.stdout.flush().await?;
        Ok(())
    }
}
