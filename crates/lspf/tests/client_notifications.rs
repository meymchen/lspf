//! Typed server-to-client notifications from handlers (issue #45).
//!
//! These tests exercise the public `Server::serve` and `Context::client`
//! seams over an in-memory transport. Assertions observe wire messages rather
//! than the engine's private outbound queue.

use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use lspf::types::notification::Notification;
use lspf::types::request::Request;
use lspf::{
    Client, ClientError, Context, LspError, RawMessage, RequestId, Server, Transport,
    TransportError, TransportReader, TransportWriter,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

#[derive(Debug, Deserialize, Serialize)]
struct StatusParams {
    message: String,
}

enum Status {}

impl Notification for Status {
    type Params = StatusParams;
    const METHOD: &'static str = "test/status";
}

enum NotifyAndReturn {}

impl Request for NotifyAndReturn {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "test/notify-and-return";
}

enum ContinueHandling {}

impl Request for ContinueHandling {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "test/continue-handling";
}

struct AppState {
    handled: Arc<AtomicUsize>,
    client: Arc<Mutex<Option<Client>>>,
}

async fn notify_and_return(
    state: Arc<AppState>,
    ctx: Context,
    value: String,
    _cancellation: lspf::CancellationToken,
) -> Result<String, LspError> {
    let client = ctx.client();
    let cloned = client.clone();
    drop(client);
    *state.client.lock().unwrap() = Some(cloned.clone());
    cloned
        .notify::<Status>(StatusParams {
            message: value.clone(),
        })
        .map_err(LspError::internal)?;
    state.handled.fetch_add(1, Ordering::SeqCst);
    Ok(value)
}

async fn continue_handling(
    state: Arc<AppState>,
    _ctx: Context,
    value: String,
    _cancellation: lspf::CancellationToken,
) -> Result<String, LspError> {
    state.handled.fetch_add(1, Ordering::SeqCst);
    Ok(value)
}

struct ChannelTransport {
    incoming: mpsc::UnboundedReceiver<RawMessage>,
    outgoing: mpsc::UnboundedSender<RawMessage>,
}

struct ChannelReader(mpsc::UnboundedReceiver<RawMessage>);
struct ChannelWriter(mpsc::UnboundedSender<RawMessage>);

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (ChannelReader(self.incoming), ChannelWriter(self.outgoing))
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

fn request(id: i32, method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn exit() -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed("exit"),
        params: Bytes::from_static(b"null"),
    }
}

async fn receive(outgoing: &mut mpsc::UnboundedReceiver<RawMessage>) -> RawMessage {
    tokio::time::timeout(std::time::Duration::from_secs(2), outgoing.recv())
        .await
        .expect("server output before watchdog timeout")
        .expect("server output channel remains open")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handler_emits_typed_notification_and_connection_handles_later_request() {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
    let handled = Arc::new(AtomicUsize::new(0));
    let captured_client = Arc::new(Mutex::new(None));
    let server = Server::builder(AppState {
        handled: Arc::clone(&handled),
        client: Arc::clone(&captured_client),
    })
    .request::<NotifyAndReturn, _, _>(notify_and_return)
    .request::<ContinueHandling, _, _>(continue_handling)
    .build()
    .expect("server builds");
    let serve = tokio::spawn(server.serve(ChannelTransport {
        incoming: incoming_rx,
        outgoing: outgoing_tx,
    }));

    incoming_tx
        .send(request(
            1,
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        ))
        .unwrap();
    assert_eq!(
        receive(&mut outgoing_rx).await.id(),
        Some(&RequestId::Number(1))
    );

    incoming_tx
        .send(request(2, NotifyAndReturn::METHOD, json!("first")))
        .unwrap();

    let notification = receive(&mut outgoing_rx).await;
    let RawMessage::Notification { method, params } = notification else {
        panic!("typed Client notification is written before the handler response");
    };
    assert_eq!(method, Status::METHOD);
    assert_eq!(
        serde_json::from_slice::<StatusParams>(&params)
            .unwrap()
            .message,
        "first"
    );

    let first_response = receive(&mut outgoing_rx).await;
    assert_eq!(first_response.id(), Some(&RequestId::Number(2)));

    incoming_tx
        .send(request(3, ContinueHandling::METHOD, json!("later")))
        .unwrap();
    let later_response = receive(&mut outgoing_rx).await;
    assert_eq!(later_response.id(), Some(&RequestId::Number(3)));

    incoming_tx.send(exit()).unwrap();
    serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");

    assert_eq!(handled.load(Ordering::SeqCst), 2);
    let client = captured_client
        .lock()
        .unwrap()
        .take()
        .expect("handler captured its connection Client");
    for value in ["after-close-1", "after-close-2"] {
        assert!(matches!(
            client.notify::<Status>(StatusParams {
                message: value.to_string(),
            }),
            Err(ClientError::OutboundClosed)
        ));
    }
}
