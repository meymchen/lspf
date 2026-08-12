use std::str::FromStr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use lspf::types::Uri;
use lspf::{
    Context, FileProvider, MemoryFileProvider, RawMessage, RequestId, Server, Transport,
    TransportError, TransportReader, TransportWriter,
};
use tokio::sync::mpsc;

#[tokio::test]
async fn memory_provider_clones_share_normalized_uri_entries_and_removals() {
    let provider = MemoryFileProvider::new();
    let clone = provider.clone();
    let encoded = Uri::from_str("file:///workspace/%61.rs").unwrap();
    let plain = Uri::from_str("FILE:///workspace/a.rs").unwrap();

    provider.insert(encoded, "first");

    assert_eq!(clone.read_text(&plain).await.as_deref(), Some("first"));
    assert_eq!(clone.remove(&plain).as_deref(), Some("first"));
    assert_eq!(provider.read_text(&plain).await, None);
}

struct ChannelTransport {
    input: mpsc::UnboundedReceiver<RawMessage>,
    output: mpsc::UnboundedSender<RawMessage>,
}

struct ChannelReader(mpsc::UnboundedReceiver<RawMessage>);
struct ChannelWriter(mpsc::UnboundedSender<RawMessage>);

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (ChannelReader(self.input), ChannelWriter(self.output))
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.0.recv().await.ok_or(TransportError::Closed)
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        self.0.send(message).map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

#[tokio::test]
async fn builder_provider_is_used_by_the_established_workspace() {
    let provider = MemoryFileProvider::new();
    let requested = Uri::from_str("file:///workspace/unopened.rs").unwrap();
    provider.insert(requested.clone(), "configured provider");
    let observed = Arc::new(Mutex::new(None));
    let hook_observed = Arc::clone(&observed);
    let hook_uri = requested.clone();
    let server = Server::builder(())
        .file_provider(provider)
        .on_initialize(move |_state, ctx: Context, _params, _ct| {
            let observed = Arc::clone(&hook_observed);
            let uri = hook_uri.clone();
            async move {
                let text = ctx.workspace().text_document(&uri).await.unwrap().text();
                *observed.lock().unwrap() = Some(text);
                Ok(None)
            }
        })
        .build()
        .unwrap();
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    let (output_tx, mut output_rx) = mpsc::unbounded_channel();
    let handle = tokio::spawn(server.serve(ChannelTransport {
        input: input_rx,
        output: output_tx,
    }));

    input_tx
        .send(RawMessage::Request {
            id: RequestId::Number(1),
            method: "initialize".into(),
            params: Bytes::from_static(br#"{"processId":null,"rootUri":null,"capabilities":{}}"#),
        })
        .unwrap();
    output_rx.recv().await.expect("initialize response");
    input_tx
        .send(RawMessage::Notification {
            method: "exit".into(),
            params: Bytes::from_static(b"null"),
        })
        .unwrap();
    drop(input_tx);
    handle.await.unwrap().unwrap();

    assert_eq!(
        observed.lock().unwrap().as_deref(),
        Some("configured provider")
    );
}
