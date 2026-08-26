//! Public tracer-bullet coverage for the client endpoint (issue #175).

use std::borrow::Cow;
use std::time::Duration;

use bytes::Bytes;
use lspf::types::notification::Notification;
use lspf::types::request::Request;
use lspf::types::{ClientCapabilities, ClientInfo, InitializeResult, ServerCapabilities};
use lspf::{
    Client, ClientConnection, ClientError, Outcome, RawMessage, RequestId, ResourcePolicy,
    Transport, TransportError, TransportReader, TransportWriter,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct EchoParams {
    text: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct EchoResult {
    text: String,
}

enum ServerEcho {}

impl Request for ServerEcho {
    type Params = EchoParams;
    type Result = EchoResult;
    const METHOD: &'static str = "test/serverEcho";
}

enum ClientEcho {}

impl Request for ClientEcho {
    type Params = EchoParams;
    type Result = EchoResult;
    const METHOD: &'static str = "test/clientEcho";
}

enum ClientEvent {}

impl Notification for ClientEvent {
    type Params = Value;
    const METHOD: &'static str = "test/clientEvent";
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

fn response(id: RequestId, result: impl Serialize) -> RawMessage {
    RawMessage::Response {
        id,
        result: Ok(Bytes::from(serde_json::to_vec(&result).unwrap())),
    }
}

async fn recv(outgoing: &mut mpsc::UnboundedReceiver<RawMessage>) -> RawMessage {
    tokio::time::timeout(Duration::from_secs(2), outgoing.recv())
        .await
        .expect("message within watchdog")
        .expect("outgoing channel remains open")
}

async fn initialize_client(
    client: Client<ChannelTransport>,
    incoming: &mpsc::UnboundedSender<RawMessage>,
    outgoing: &mut mpsc::UnboundedReceiver<RawMessage>,
    inspect_params: impl FnOnce(&Bytes),
) -> ClientConnection {
    let connecting = tokio::spawn(client.connect());
    let initialize = recv(outgoing).await;
    let (id, params) = match initialize {
        RawMessage::Request {
            id,
            method: Cow::Borrowed("initialize"),
            params,
        } => (id, params),
        other => panic!("expected initialize request, got {other:?}"),
    };
    inspect_params(&params);
    incoming
        .send(response(
            id,
            InitializeResult {
                capabilities: ServerCapabilities::default(),
                server_info: None,
            },
        ))
        .unwrap();
    connecting.await.unwrap().expect("client initializes")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_initializes_and_completes_one_typed_exchange_each_way() {
    let capabilities = ClientCapabilities {
        workspace: Some(Default::default()),
        ..Default::default()
    };
    let client_info = ClientInfo {
        name: "lspf integration client".into(),
        version: Some("1.2.3".into()),
    };
    let initialization_options = json!({ "profile": "integration" });
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();

    let client = Client::builder(capabilities.clone())
        .client_info(client_info.clone())
        .initialization_options(initialization_options.clone())
        .request::<ClientEcho, _, _>(|params, cancellation| async move {
            assert!(!cancellation.is_cancelled());
            Ok(EchoResult {
                text: format!("client: {}", params.text),
            })
        })
        .build(ChannelTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
        })
        .expect("client builds");

    let connection = initialize_client(client, &incoming_tx, &mut outgoing_rx, |params| {
        let params: Value = serde_json::from_slice(params).unwrap();
        assert_eq!(
            params["capabilities"],
            serde_json::to_value(capabilities).unwrap()
        );
        assert_eq!(
            params["clientInfo"],
            serde_json::to_value(client_info).unwrap()
        );
        assert_eq!(params["initializationOptions"], initialization_options);
    })
    .await;
    let server = connection.server();
    let serving = tokio::spawn(connection.serve());

    assert!(matches!(
        recv(&mut outgoing_rx).await,
        RawMessage::Notification {
            method: Cow::Borrowed("initialized"),
            ..
        }
    ));

    server
        .notify::<ClientEvent>(json!({ "ready": true }))
        .expect("typed notification is accepted");
    assert!(matches!(
        recv(&mut outgoing_rx).await,
        RawMessage::Notification {
            method: Cow::Borrowed("test/clientEvent"),
            ..
        }
    ));

    let requesting = tokio::spawn({
        let server = server.clone();
        async move {
            server
                .request::<ServerEcho>(EchoParams {
                    text: "outbound".into(),
                })
                .await
        }
    });
    let outbound = recv(&mut outgoing_rx).await;
    let outbound_id = match outbound {
        RawMessage::Request {
            id,
            method: Cow::Borrowed("test/serverEcho"),
            ..
        } => id,
        other => panic!("expected typed server request, got {other:?}"),
    };
    incoming_tx
        .send(response(
            outbound_id,
            EchoResult {
                text: "server: outbound".into(),
            },
        ))
        .unwrap();
    assert_eq!(
        requesting.await.unwrap().expect("typed request completes"),
        EchoResult {
            text: "server: outbound".into()
        }
    );

    incoming_tx
        .send(RawMessage::Request {
            id: RequestId::Number(77),
            method: Cow::Borrowed("test/clientEcho"),
            params: Bytes::from_static(br#"{"text":"inbound"}"#),
        })
        .unwrap();
    let reverse = recv(&mut outgoing_rx).await;
    match reverse {
        RawMessage::Response {
            id: RequestId::Number(77),
            result: Ok(result),
        } => assert_eq!(
            serde_json::from_slice::<EchoResult>(&result).unwrap(),
            EchoResult {
                text: "client: inbound".into()
            }
        ),
        other => panic!("expected typed reverse response, got {other:?}"),
    }

    drop(incoming_tx);
    assert_eq!(serving.await.unwrap().unwrap(), Outcome::TransportClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_uses_shared_correlation_and_close_for_outbound_requests() {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
    let client = Client::builder(ClientCapabilities::default())
        .build(ChannelTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
        })
        .expect("client builds");
    let connection = initialize_client(client, &incoming_tx, &mut outgoing_rx, |_| {}).await;
    let server = connection.server();
    let serving = tokio::spawn(connection.serve());
    let _initialized = recv(&mut outgoing_rx).await;

    let first = tokio::spawn({
        let server = server.clone();
        async move {
            server
                .request::<ServerEcho>(EchoParams {
                    text: "first".into(),
                })
                .await
        }
    });
    let second = tokio::spawn({
        let server = server.clone();
        async move {
            server
                .request::<ServerEcho>(EchoParams {
                    text: "second".into(),
                })
                .await
        }
    });
    let first_message = recv(&mut outgoing_rx).await;
    let second_message = recv(&mut outgoing_rx).await;
    let request_parts = |message: RawMessage| match message {
        RawMessage::Request { id, params, .. } => {
            (id, serde_json::from_slice::<EchoParams>(&params).unwrap())
        }
        other => panic!("expected echo request, got {other:?}"),
    };
    let (first_id, first_params) = request_parts(first_message);
    let (second_id, second_params) = request_parts(second_message);
    assert_ne!(first_id, second_id);

    incoming_tx
        .send(response(
            second_id,
            EchoResult {
                text: format!("{} response", second_params.text),
            },
        ))
        .unwrap();
    incoming_tx
        .send(response(
            first_id,
            EchoResult {
                text: format!("{} response", first_params.text),
            },
        ))
        .unwrap();
    assert_eq!(first.await.unwrap().unwrap().text, "first response");
    assert_eq!(second.await.unwrap().unwrap().text, "second response");

    let pending = tokio::spawn({
        let server = server.clone();
        async move {
            server
                .request::<ServerEcho>(EchoParams {
                    text: "never answered".into(),
                })
                .await
        }
    });
    let _pending_message = recv(&mut outgoing_rx).await;
    drop(incoming_tx);
    assert_eq!(serving.await.unwrap().unwrap(), Outcome::TransportClosed);
    assert!(matches!(
        pending.await.unwrap(),
        Err(ClientError::Cancelled)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_uses_shared_admission_and_cancellation_for_reverse_requests() {
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
    let client = Client::builder(ClientCapabilities::default())
        .resource_policy(ResourcePolicy {
            max_inbound_requests: 1,
            ..ResourcePolicy::default()
        })
        .request::<ClientEcho, _, _>(move |_params, cancellation| {
            let started_tx = started_tx.clone();
            async move {
                started_tx.send(cancellation.clone()).unwrap();
                cancellation.cancelled().await;
                Err(lspf::LspError::RequestCancelled)
            }
        })
        .build(ChannelTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
        })
        .expect("client builds");
    let connection = initialize_client(client, &incoming_tx, &mut outgoing_rx, |_| {}).await;
    let serving = tokio::spawn(connection.serve());
    let _initialized = recv(&mut outgoing_rx).await;

    incoming_tx
        .send(RawMessage::Request {
            id: RequestId::Number(10),
            method: Cow::Borrowed(ClientEcho::METHOD),
            params: Bytes::from_static(br#"{"text":"first"}"#),
        })
        .unwrap();
    let cancellation = tokio::time::timeout(Duration::from_secs(2), started_rx.recv())
        .await
        .unwrap()
        .unwrap();
    incoming_tx
        .send(RawMessage::Request {
            id: RequestId::Number(11),
            method: Cow::Borrowed(ClientEcho::METHOD),
            params: Bytes::from_static(br#"{"text":"over limit"}"#),
        })
        .unwrap();
    match recv(&mut outgoing_rx).await {
        RawMessage::Response {
            id: RequestId::Number(11),
            result: Err(error),
        } => {
            assert_eq!(error.code, -32802);
            assert_eq!(error.message, "inbound request capacity exhausted");
        }
        other => panic!("expected admission rejection, got {other:?}"),
    }

    incoming_tx
        .send(RawMessage::Notification {
            method: Cow::Borrowed("$/cancelRequest"),
            params: Bytes::from_static(br#"{"id":10}"#),
        })
        .unwrap();
    match recv(&mut outgoing_rx).await {
        RawMessage::Response {
            id: RequestId::Number(10),
            result: Err(error),
        } => assert_eq!(error.code, -32800),
        other => panic!("expected cancellation response, got {other:?}"),
    }
    tokio::time::timeout(Duration::from_secs(2), cancellation.cancelled())
        .await
        .expect("handler observes cancellation");

    drop(incoming_tx);
    assert_eq!(serving.await.unwrap().unwrap(), Outcome::TransportClosed);
}
