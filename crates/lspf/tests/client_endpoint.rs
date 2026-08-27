//! Public tracer-bullet coverage for the client endpoint (issue #175).

use std::borrow::Cow;
use std::time::Duration;

use bytes::Bytes;
use lspf::types::notification::{Notification, Progress};
use lspf::types::request::{RegisterCapability, Request, WorkDoneProgressCreate};
use lspf::types::{ClientCapabilities, ClientInfo, InitializeResult, ServerCapabilities};
use lspf::{
    Client, ClientBuilder, ClientConnection, ClientError, Outcome, RawMessage, RequestId,
    ResourcePolicy, ServerHandle, Transport, TransportError, TransportReader, TransportWriter,
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressEchoParams {
    text: String,
    work_done_token: Option<lspf::types::ProgressToken>,
}

enum ServerEcho {}

impl Request for ServerEcho {
    type Params = EchoParams;
    type Result = EchoResult;
    const METHOD: &'static str = "test/serverEcho";
}

enum ServerProgressEcho {}

impl Request for ServerProgressEcho {
    type Params = ProgressEchoParams;
    type Result = EchoResult;
    const METHOD: &'static str = "test/serverProgressEcho";
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

enum DuplicateInitialize {}

impl Request for DuplicateInitialize {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "initialize";
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

struct ConnectedClient {
    incoming: mpsc::UnboundedSender<RawMessage>,
    outgoing: mpsc::UnboundedReceiver<RawMessage>,
    server: ServerHandle,
    serving: tokio::task::JoinHandle<lspf::Result<Outcome>>,
}

impl ConnectedClient {
    async fn start(builder: ClientBuilder) -> Self {
        let (incoming, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing) = mpsc::unbounded_channel();
        let client = builder
            .build(ChannelTransport {
                incoming: incoming_rx,
                outgoing: outgoing_tx,
            })
            .expect("client builds");
        let connection = initialize_client(client, &incoming, &mut outgoing, |_| {}).await;
        let server = connection.server();
        let serving = tokio::spawn(connection.serve());
        assert!(matches!(
            recv(&mut outgoing).await,
            RawMessage::Notification {
                method: Cow::Borrowed("initialized"),
                ..
            }
        ));
        Self {
            incoming,
            outgoing,
            server,
            serving,
        }
    }

    async fn recv(&mut self) -> RawMessage {
        recv(&mut self.outgoing).await
    }

    fn send(&self, message: RawMessage) {
        self.incoming.send(message).unwrap();
    }

    fn spawn_shutdown(&self) -> tokio::task::JoinHandle<Result<(), ClientError>> {
        let server = self.server.clone();
        tokio::spawn(async move { server.shutdown().await })
    }

    async fn recv_request_id(&mut self, expected_method: &'static str) -> RequestId {
        match self.recv().await {
            RawMessage::Request { id, method, .. } if method == expected_method => id,
            other => panic!("expected {expected_method} request, got {other:?}"),
        }
    }

    async fn outcome(self) -> Outcome {
        self.serving.await.unwrap().unwrap()
    }
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
        .request::<ClientEcho, _, _>(|_ctx, params, cancellation| async move {
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
async fn client_rejects_reverse_requests_before_and_after_initialize() {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
    let client = Client::builder(ClientCapabilities::default())
        .build(ChannelTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
        })
        .expect("client builds");

    let connecting = tokio::spawn(client.connect());
    let initialize = recv(&mut outgoing_rx).await;
    let initialize_id = initialize.id().cloned().expect("initialize request id");

    incoming_tx
        .send(RawMessage::Request {
            id: RequestId::Number(70),
            method: Cow::Borrowed(ClientEcho::METHOD),
            params: Bytes::from_static(br#"{"text":"too early"}"#),
        })
        .unwrap();
    match recv(&mut outgoing_rx).await {
        RawMessage::Response {
            id: RequestId::Number(70),
            result: Err(error),
        } => assert_eq!(error.code, -32002),
        other => panic!("expected pre-initialize rejection, got {other:?}"),
    }

    incoming_tx
        .send(RawMessage::Request {
            id: RequestId::Number(71),
            method: Cow::Borrowed("initialize"),
            params: Bytes::from_static(b"{}"),
        })
        .unwrap();
    match recv(&mut outgoing_rx).await {
        RawMessage::Response {
            id: RequestId::Number(71),
            result: Err(error),
        } => assert_eq!(error.code, -32600),
        other => panic!("expected in-progress duplicate-initialize rejection, got {other:?}"),
    }

    incoming_tx
        .send(response(
            initialize_id,
            InitializeResult {
                capabilities: ServerCapabilities::default(),
                server_info: None,
            },
        ))
        .unwrap();
    let connection = connecting.await.unwrap().expect("client initializes");
    let serving = tokio::spawn(connection.serve());
    let _initialized = recv(&mut outgoing_rx).await;

    incoming_tx
        .send(RawMessage::Request {
            id: RequestId::Number(72),
            method: Cow::Borrowed("initialize"),
            params: Bytes::from_static(b"{}"),
        })
        .unwrap();
    match recv(&mut outgoing_rx).await {
        RawMessage::Response {
            id: RequestId::Number(72),
            result: Err(error),
        } => assert_eq!(error.code, -32600),
        other => panic!("expected duplicate-initialize rejection, got {other:?}"),
    }

    drop(incoming_tx);
    assert_eq!(serving.await.unwrap().unwrap(), Outcome::TransportClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_shutdown_refuses_later_work_and_exit_closes_cleanly() {
    let mut client = ConnectedClient::start(Client::builder(ClientCapabilities::default())).await;

    assert!(matches!(
        client
            .server
            .request::<DuplicateInitialize>(json!({}))
            .await,
        Err(ClientError::InvalidLifecycle { .. })
    ));

    let shutting_down = client.spawn_shutdown();
    let shutdown_id = client.recv_request_id("shutdown").await;
    client.send(response(shutdown_id, Value::Null));
    shutting_down
        .await
        .unwrap()
        .expect("shutdown response completes lifecycle transition");

    assert!(matches!(
        client.server.notify::<ClientEvent>(json!({})),
        Err(ClientError::InvalidLifecycle { .. })
    ));
    assert!(matches!(
        client.server.shutdown().await,
        Err(ClientError::InvalidLifecycle { .. })
    ));

    client.send(RawMessage::Request {
        id: RequestId::Number(80),
        method: Cow::Borrowed(ClientEcho::METHOD),
        params: Bytes::from_static(br#"{"text":"too late"}"#),
    });
    match client.recv().await {
        RawMessage::Response {
            id: RequestId::Number(80),
            result: Err(error),
        } => assert_eq!(error.code, -32600),
        other => panic!("expected post-shutdown rejection, got {other:?}"),
    }

    client.server.exit().expect("exit follows shutdown");
    assert!(matches!(
        client.recv().await,
        RawMessage::Notification {
            method: Cow::Borrowed("exit"),
            ..
        }
    ));
    assert_eq!(client.outcome().await, Outcome::Exit { code: 0 });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_shutdown_resolves_pending_work_in_both_directions() {
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let mut client = ConnectedClient::start(
        Client::builder(ClientCapabilities::default()).request::<ClientEcho, _, _>(
            move |_ctx, _params, cancellation| {
                let started_tx = started_tx.clone();
                async move {
                    started_tx.send(cancellation.clone()).unwrap();
                    cancellation.cancelled().await;
                    Err(lspf::LspError::RequestCancelled)
                }
            },
        ),
    )
    .await;

    let pending = tokio::spawn({
        let server = client.server.clone();
        async move {
            server
                .request::<ServerEcho>(EchoParams {
                    text: "pending".into(),
                })
                .await
        }
    });
    let _pending_request = client.recv().await;

    client.send(RawMessage::Request {
        id: RequestId::Number(90),
        method: Cow::Borrowed(ClientEcho::METHOD),
        params: Bytes::from_static(br#"{"text":"reverse pending"}"#),
    });
    let reverse_cancellation = tokio::time::timeout(Duration::from_secs(2), started_rx.recv())
        .await
        .expect("reverse handler starts within watchdog")
        .expect("reverse handler reports its cancellation token");

    let shutting_down = client.spawn_shutdown();
    let shutdown_id = client.recv_request_id("shutdown").await;
    client.send(response(shutdown_id, Value::Null));
    shutting_down.await.unwrap().unwrap();

    assert!(matches!(
        pending.await.unwrap(),
        Err(ClientError::Cancelled)
    ));
    match client.recv().await {
        RawMessage::Response {
            id: RequestId::Number(90),
            result: Err(error),
        } => assert_eq!(error.code, -32800),
        other => panic!("expected cancelled reverse response, got {other:?}"),
    }
    assert!(reverse_cancellation.is_cancelled());

    client.server.exit().unwrap();
    let _exit = client.recv().await;
    assert_eq!(client.outcome().await, Outcome::Exit { code: 0 });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_disconnect_is_idempotent_and_resolves_pending_work() {
    let mut client = ConnectedClient::start(Client::builder(ClientCapabilities::default())).await;

    assert!(matches!(
        client.server.exit(),
        Err(ClientError::InvalidLifecycle { .. })
    ));
    let pending = tokio::spawn({
        let server = client.server.clone();
        async move {
            server
                .request::<ServerEcho>(EchoParams {
                    text: "pending".into(),
                })
                .await
        }
    });
    let _pending_request = client.recv().await;

    client.server.disconnect();
    client.server.disconnect();

    assert!(matches!(
        pending.await.unwrap(),
        Err(ClientError::Cancelled)
    ));
    let server = client.server.clone();
    assert_eq!(client.outcome().await, Outcome::TransportClosed);
    assert!(matches!(
        server.notify::<ClientEvent>(json!({})),
        Err(ClientError::InvalidLifecycle { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_initialize_uses_the_shared_outbound_deadline() {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
    let client = Client::builder(ClientCapabilities::default())
        .resource_policy(ResourcePolicy {
            outbound_request_timeout: Some(Duration::from_millis(20)),
            ..ResourcePolicy::default()
        })
        .build(ChannelTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
        })
        .expect("client builds");

    let connecting = tokio::spawn(client.connect());
    let initialize_id = match recv(&mut outgoing_rx).await {
        RawMessage::Request {
            id,
            method: Cow::Borrowed("initialize"),
            ..
        } => id,
        other => panic!("expected initialize request, got {other:?}"),
    };
    assert!(matches!(
        connecting.await.unwrap(),
        Err(lspf::Error::Client(ClientError::Timeout))
    ));
    match recv(&mut outgoing_rx).await {
        RawMessage::Notification { method, params }
            if method == "$/cancelRequest"
                && serde_json::from_slice::<Value>(&params).unwrap()["id"]
                    == serde_json::to_value(initialize_id).unwrap() => {}
        other => panic!("expected initialize cancellation, got {other:?}"),
    }
    drop(incoming_tx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_shutdown_timeout_restores_running_and_allows_retry() {
    let mut client = ConnectedClient::start(
        Client::builder(ClientCapabilities::default()).resource_policy(ResourcePolicy {
            outbound_request_timeout: Some(Duration::from_millis(20)),
            ..ResourcePolicy::default()
        }),
    )
    .await;

    let first_shutdown = client.spawn_shutdown();
    let first_id = client.recv_request_id("shutdown").await;
    assert!(matches!(
        first_shutdown.await.unwrap(),
        Err(ClientError::Timeout)
    ));
    match client.recv().await {
        RawMessage::Notification { method, params }
            if method == "$/cancelRequest"
                && serde_json::from_slice::<Value>(&params).unwrap()["id"]
                    == serde_json::to_value(&first_id).unwrap() => {}
        other => panic!("expected timed-out shutdown cancellation, got {other:?}"),
    }

    client
        .server
        .notify::<ClientEvent>(json!({ "stillRunning": true }))
        .expect("failed shutdown leaves the client running");
    let _notification = client.recv().await;

    let retry = client.spawn_shutdown();
    let retry_id = client.recv_request_id("shutdown").await;
    assert_ne!(retry_id, first_id, "timed-out IDs are never reused");
    client.send(response(retry_id, Value::Null));
    retry.await.unwrap().expect("shutdown retry succeeds");

    client.server.exit().unwrap();
    let _exit = client.recv().await;
    assert_eq!(client.outcome().await, Outcome::Exit { code: 0 });
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
        .request::<ClientEcho, _, _>(move |_ctx, _params, cancellation| {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_context_supports_nested_typed_calls_progress_and_dynamic_registration() {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    let (create_tx, mut create_rx) = mpsc::unbounded_channel();
    let client = Client::builder(ClientCapabilities::default())
        .notification::<Progress, _, _>(move |ctx, params| {
            let progress_tx = progress_tx.clone();
            async move {
                let result = if params.token == lspf::types::NumberOrString::Number(9)
                    && matches!(
                        &params.value,
                        lspf::types::ProgressParamsValue::WorkDone(
                            lspf::types::WorkDoneProgress::Begin(_)
                        )
                    ) {
                    Some(
                        ctx.server()
                            .request::<ServerEcho>(EchoParams {
                                text: "progress".into(),
                            })
                            .await
                            .expect("progress handler can await a nested typed request"),
                    )
                } else {
                    None
                };
                progress_tx
                    .send((ctx.request_id().cloned(), params, result))
                    .unwrap();
            }
        })
        .request::<WorkDoneProgressCreate, _, _>(move |_ctx, params, cancellation| {
            let create_tx = create_tx.clone();
            async move {
                assert!(!cancellation.is_cancelled());
                create_tx.send(params.token).unwrap();
                Ok(())
            }
        })
        .request::<RegisterCapability, _, _>(|ctx, _params, cancellation| async move {
            assert!(!cancellation.is_cancelled());
            assert_eq!(ctx.request_id(), Some(&RequestId::Number(70)));
            let server = ctx.server();
            server
                .notify::<ClientEvent>(json!({ "fromHandler": true }))
                .expect("reverse handler sends a typed notification");

            let first = server.request::<ServerEcho>(EchoParams {
                text: "first".into(),
            });
            let second = server.request::<ServerEcho>(EchoParams {
                text: "second".into(),
            });
            let (first, second) = tokio::join!(first, second);
            assert_eq!(
                first.expect("first nested request completes").text,
                "first response"
            );
            assert_eq!(
                second.expect("second nested request completes").text,
                "second response"
            );
            Ok(())
        })
        .build(ChannelTransport {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
        })
        .expect("client builds");
    let connection = initialize_client(client, &incoming_tx, &mut outgoing_rx, |_| {}).await;
    let server = connection.server();
    let serving = tokio::spawn(connection.serve());
    let _initialized = recv(&mut outgoing_rx).await;

    incoming_tx
        .send(RawMessage::Request {
            id: RequestId::Number(60),
            method: Cow::Borrowed(WorkDoneProgressCreate::METHOD),
            params: Bytes::from_static(br#"{"token":9}"#),
        })
        .unwrap();
    assert!(matches!(
        recv(&mut outgoing_rx).await,
        RawMessage::Response {
            id: RequestId::Number(60),
            result: Ok(_),
        }
    ));
    assert_eq!(
        create_rx.recv().await.unwrap(),
        lspf::types::NumberOrString::Number(9)
    );

    incoming_tx
        .send(RawMessage::Request {
            id: RequestId::Number(61),
            method: Cow::Borrowed(WorkDoneProgressCreate::METHOD),
            params: Bytes::from_static(br#"{"token":9}"#),
        })
        .unwrap();
    match recv(&mut outgoing_rx).await {
        RawMessage::Response {
            id: RequestId::Number(61),
            result: Err(error),
        } => assert_eq!(error.code, -32602),
        other => panic!("expected duplicate progress-token rejection, got {other:?}"),
    }
    assert!(create_rx.try_recv().is_err());

    incoming_tx
        .send(RawMessage::Notification {
            method: Cow::Borrowed(Progress::METHOD),
            params: Bytes::from_static(
                br#"{"token":9,"value":{"kind":"begin","title":"Indexing"}}"#,
            ),
        })
        .unwrap();
    incoming_tx
        .send(RawMessage::Notification {
            method: Cow::Borrowed(Progress::METHOD),
            params: Bytes::from_static(br#"{"token":9,"value":{"kind":"end","message":"done"}}"#),
        })
        .unwrap();
    let (progress_request_id, progress_request_params) = match recv(&mut outgoing_rx).await {
        RawMessage::Request { id, params, .. } => {
            (id, serde_json::from_slice::<EchoParams>(&params).unwrap())
        }
        other => panic!("expected progress handler's nested request, got {other:?}"),
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(20), progress_rx.recv())
            .await
            .is_err(),
        "progress end waits for the begin handler on the same token"
    );
    incoming_tx
        .send(response(
            progress_request_id,
            EchoResult {
                text: format!("{} response", progress_request_params.text),
            },
        ))
        .unwrap();
    let (request_id, progress, nested_result) =
        tokio::time::timeout(Duration::from_secs(2), progress_rx.recv())
            .await
            .expect("progress handler completes within watchdog")
            .expect("progress handler runs exactly once");
    assert!(request_id.is_none());
    assert_eq!(progress.token, lspf::types::NumberOrString::Number(9));
    assert_eq!(nested_result.unwrap().text, "progress response");
    let (_, _, nested_result) = tokio::time::timeout(Duration::from_secs(2), progress_rx.recv())
        .await
        .expect("progress end completes within watchdog")
        .expect("progress end runs exactly once");
    assert!(nested_result.is_none());

    incoming_tx
        .send(RawMessage::Notification {
            method: Cow::Borrowed(Progress::METHOD),
            params: Bytes::from_static(
                br#"{"token":9,"value":{"kind":"report","message":"too late"}}"#,
            ),
        })
        .unwrap();

    incoming_tx
        .send(RawMessage::Request {
            id: RequestId::Number(70),
            method: Cow::Borrowed(RegisterCapability::METHOD),
            params: Bytes::from_static(br#"{"registrations":[]}"#),
        })
        .unwrap();
    assert!(matches!(
        recv(&mut outgoing_rx).await,
        RawMessage::Notification {
            method: Cow::Borrowed("test/clientEvent"),
            ..
        }
    ));

    let first_message = recv(&mut outgoing_rx).await;
    let second_message = recv(&mut outgoing_rx).await;
    let request_parts = |message: RawMessage| match message {
        RawMessage::Request { id, params, .. } => {
            (id, serde_json::from_slice::<EchoParams>(&params).unwrap())
        }
        other => panic!("expected nested request, got {other:?}"),
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
    match recv(&mut outgoing_rx).await {
        RawMessage::Response {
            id: RequestId::Number(70),
            result: Ok(result),
        } => assert_eq!(
            serde_json::from_slice::<Value>(&result).unwrap(),
            Value::Null
        ),
        other => panic!("expected one dynamic-registration response, got {other:?}"),
    }
    tokio::task::yield_now().await;
    assert!(
        progress_rx.try_recv().is_err(),
        "an ended progress token ignores late notifications"
    );

    let requesting_with_progress = tokio::spawn({
        let server = server.clone();
        async move {
            server
                .request::<ServerProgressEcho>(ProgressEchoParams {
                    text: "client initiated".into(),
                    work_done_token: Some(lspf::types::NumberOrString::Number(10)),
                })
                .await
        }
    });
    let progress_request_id = match recv(&mut outgoing_rx).await {
        RawMessage::Request { id, method, .. } if method.as_ref() == ServerProgressEcho::METHOD => {
            id
        }
        other => panic!("expected client-initiated progress request, got {other:?}"),
    };
    for params in [
        br#"{"token":10,"value":{"kind":"begin","title":"Client work"}}"#.as_slice(),
        br#"{"token":10,"value":{"kind":"end","message":"done"}}"#.as_slice(),
    ] {
        incoming_tx
            .send(RawMessage::Notification {
                method: Cow::Borrowed(Progress::METHOD),
                params: Bytes::copy_from_slice(params),
            })
            .unwrap();
    }
    let (_, begin, _) = progress_rx.recv().await.unwrap();
    let (_, end, _) = progress_rx.recv().await.unwrap();
    assert!(matches!(
        begin.value,
        lspf::types::ProgressParamsValue::WorkDone(lspf::types::WorkDoneProgress::Begin(_))
    ));
    assert!(matches!(
        end.value,
        lspf::types::ProgressParamsValue::WorkDone(lspf::types::WorkDoneProgress::End(_))
    ));
    incoming_tx
        .send(response(
            progress_request_id,
            EchoResult {
                text: "client initiated response".into(),
            },
        ))
        .unwrap();
    assert_eq!(
        requesting_with_progress.await.unwrap().unwrap().text,
        "client initiated response"
    );

    drop(incoming_tx);
    assert_eq!(serving.await.unwrap().unwrap(), Outcome::TransportClosed);
}
