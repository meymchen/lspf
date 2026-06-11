use futures_util::StreamExt;
use tokio::io::{AsyncWriteExt, Stdin, Stdout};
use tokio_util::codec::FramedRead;

use super::framing::ContentLengthCodec;
use super::{TransportError, envelope};
use crate::raw::RawMessage;
use crate::transport::Transport;

pub struct StdioTransport {
    framed_in: FramedRead<Stdin, ContentLengthCodec>,
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
            framed_in: FramedRead::new(tokio::io::stdin(), ContentLengthCodec::default()),
            stdout: tokio::io::stdout(),
        }
    }
}

impl Transport for StdioTransport {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        let body = self
            .framed_in
            .next()
            .await
            .ok_or(TransportError::Closed)??;
        envelope::parse(body)
    }

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
