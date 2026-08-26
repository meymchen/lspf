use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use lspf::types::notification::Notification;
use lspf::types::request::Request;
use lspf::{
    BuildError, CancellationToken, ConnectionFailure, ConnectionFailureCategory,
    ConnectionRequestId, Context, IncomingCall, JsonRpcError, Layer, Next, RawMessage, RequestId,
    ResourcePolicy, Server, ServiceFuture, Transport, TransportError, TransportReader,
    TransportWriter,
};

enum Echo {}

impl Request for Echo {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "test/echo";
}

enum Stop {}

impl Notification for Stop {
    type Params = ();
    const METHOD: &'static str = "test/stop";
}

enum ClientQuery {}

impl Request for ClientQuery {
    type Params = ();
    type Result = String;
    const METHOD: &'static str = "test/clientQuery";
}

#[derive(Default)]
struct TestTransport {
    inbound: VecDeque<Result<RawMessage, TransportError>>,
    outbound: Arc<Mutex<Vec<RawMessage>>>,
}

struct TestReader(VecDeque<Result<RawMessage, TransportError>>);
struct TestWriter(Arc<Mutex<Vec<RawMessage>>>);

#[derive(Clone, Copy)]
enum WriterFailure {
    Send,
    Shutdown,
}

struct FailingWriterTransport {
    inbound: VecDeque<Result<RawMessage, TransportError>>,
    failure: WriterFailure,
}

struct FailingWriter(WriterFailure);

struct SlowWriterTransport {
    inbound: VecDeque<Result<RawMessage, TransportError>>,
}

struct SlowWriter;

struct CorrelatedResponseTransport {
    outbound_request: Arc<tokio::sync::Notify>,
    handler_response: Arc<tokio::sync::Notify>,
}

struct CorrelatedResponseReader {
    stage: u8,
    outbound_request: Arc<tokio::sync::Notify>,
    handler_response: Arc<tokio::sync::Notify>,
}

struct CorrelatedResponseWriter {
    outbound_request: Arc<tokio::sync::Notify>,
    handler_response: Arc<tokio::sync::Notify>,
}

struct CountingLayer(Arc<AtomicUsize>);

impl Layer<()> for CountingLayer {
    fn call(&self, call: IncomingCall<()>, next: Next<()>) -> ServiceFuture {
        self.0.fetch_add(1, Ordering::Relaxed);
        next.call(call)
    }
}

impl Transport for SlowWriterTransport {
    type Reader = TestReader;
    type Writer = SlowWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (TestReader(self.inbound), SlowWriter)
    }
}

impl TransportWriter for SlowWriter {
    async fn send(&mut self, _message: RawMessage) -> Result<(), TransportError> {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        Ok(())
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

impl Transport for CorrelatedResponseTransport {
    type Reader = CorrelatedResponseReader;
    type Writer = CorrelatedResponseWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            CorrelatedResponseReader {
                stage: 0,
                outbound_request: Arc::clone(&self.outbound_request),
                handler_response: Arc::clone(&self.handler_response),
            },
            CorrelatedResponseWriter {
                outbound_request: self.outbound_request,
                handler_response: self.handler_response,
            },
        )
    }
}

impl TransportReader for CorrelatedResponseReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        let message = match self.stage {
            0 => initialize(1),
            1 => request(2, Echo::METHOD, serde_json::json!("trigger")),
            2 => {
                self.outbound_request.notified().await;
                RawMessage::Response {
                    id: RequestId::Number(1),
                    result: Ok(Bytes::from_static(b"42")),
                }
            }
            3 => {
                self.handler_response.notified().await;
                notification("exit", serde_json::Value::Null)
            }
            _ => return Err(TransportError::Closed),
        };
        self.stage += 1;
        Ok(message)
    }
}

impl TransportWriter for CorrelatedResponseWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        match message {
            RawMessage::Request { ref method, .. } if method == ClientQuery::METHOD => {
                self.outbound_request.notify_one();
            }
            RawMessage::Response {
                id: RequestId::Number(2),
                ..
            } => self.handler_response.notify_one(),
            _ => {}
        }
        Ok(())
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

impl Transport for FailingWriterTransport {
    type Reader = TestReader;
    type Writer = FailingWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (TestReader(self.inbound), FailingWriter(self.failure))
    }
}

impl TransportWriter for FailingWriter {
    async fn send(&mut self, _message: RawMessage) -> Result<(), TransportError> {
        match self.0 {
            WriterFailure::Send => Err(TransportError::Io(std::io::Error::other("secret send"))),
            WriterFailure::Shutdown => Ok(()),
        }
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        match self.0 {
            WriterFailure::Send => Ok(()),
            WriterFailure::Shutdown => {
                Err(TransportError::Io(std::io::Error::other("secret shutdown")))
            }
        }
    }
}

impl Transport for TestTransport {
    type Reader = TestReader;
    type Writer = TestWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (TestReader(self.inbound), TestWriter(self.outbound))
    }
}

impl TransportReader for TestReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        tokio::task::yield_now().await;
        self.0.pop_front().unwrap_or(Err(TransportError::Closed))
    }
}

impl TransportWriter for TestWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        self.0.lock().unwrap().push(message);
        Ok(())
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

fn notification(method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn initialize(id: i32) -> RawMessage {
    request(id, "initialize", serde_json::json!({"capabilities": {}}))
}

#[tokio::test]
async fn isolated_panics_report_stable_non_sensitive_context_once() {
    let failures = Arc::new(Mutex::new(Vec::<ConnectionFailure>::new()));
    let recorded = Arc::clone(&failures);
    let server = Server::builder(())
        .request::<Echo, _, _>(|_, _: Context, _, _: CancellationToken| async move {
            panic!("secret panic payload")
        })
        .notification::<Stop, _, _>(|_, _, ()| async {})
        .on_error(move |failure| recorded.lock().unwrap().push(failure))
        .build()
        .unwrap();
    let outbound = Arc::new(Mutex::new(Vec::new()));
    let transport = TestTransport {
        inbound: VecDeque::from([
            Ok(initialize(1)),
            Ok(request(2, Echo::METHOD, serde_json::json!("secret params"))),
            Ok(notification("exit", serde_json::Value::Null)),
        ]),
        outbound: Arc::clone(&outbound),
    };

    let outcome = server.serve(transport).await.unwrap();
    assert_eq!(outcome.code(), 1);
    let failures = failures.lock().unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(
        failures[0].category,
        ConnectionFailureCategory::PanicIsolation
    );
    assert_eq!(
        failures[0].context.direction,
        Some(lspf::ConnectionDirection::Inbound)
    );
    assert_eq!(failures[0].context.method.as_deref(), Some(Echo::METHOD));
    assert_eq!(
        failures[0].context.request_id,
        Some(ConnectionRequestId::Number(2))
    );
    assert_ne!(failures[0].context.connection_id, 0);
    assert!(!format!("{failures:?}").contains("secret"));
}

#[tokio::test]
async fn peer_controlled_string_request_ids_are_redacted() {
    let failures = Arc::new(Mutex::new(Vec::<ConnectionFailure>::new()));
    let recorded = Arc::clone(&failures);
    let server = Server::builder(())
        .on_error(move |failure| recorded.lock().unwrap().push(failure))
        .build()
        .unwrap();
    let transport = TestTransport {
        inbound: VecDeque::from([
            Ok(initialize(1)),
            Ok(RawMessage::Request {
                id: RequestId::String("secret-user-id".into()),
                method: Cow::Borrowed("secret-method"),
                params: Bytes::from_static(b"not-json"),
            }),
            Ok(notification("exit", serde_json::Value::Null)),
        ]),
        outbound: Arc::new(Mutex::new(Vec::new())),
    };

    server.serve(transport).await.unwrap();
    let failures = failures.lock().unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(
        failures[0].context.request_id,
        Some(ConnectionRequestId::String)
    );
    assert!(failures[0].context.method.is_none());
    assert!(!format!("{failures:?}").contains("secret-user-id"));
    assert!(!format!("{failures:?}").contains("secret-method"));
}

#[tokio::test]
async fn malformed_correlated_response_reports_protocol_failure_once() {
    let failures = Arc::new(Mutex::new(Vec::<ConnectionFailure>::new()));
    let recorded = Arc::clone(&failures);
    let server = Server::builder(())
        .request::<Echo, _, _>(|_, ctx: Context, value, _| async move {
            let result = ctx.client().request::<ClientQuery>(()).await;
            assert!(matches!(result, Err(lspf::ClientError::Deserialize(_))));
            Ok(value)
        })
        .on_error(move |failure| recorded.lock().unwrap().push(failure))
        .build()
        .unwrap();
    let transport = CorrelatedResponseTransport {
        outbound_request: Arc::new(tokio::sync::Notify::new()),
        handler_response: Arc::new(tokio::sync::Notify::new()),
    };

    server.serve(transport).await.unwrap();
    let failures = failures.lock().unwrap();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].category, ConnectionFailureCategory::Protocol);
    assert_eq!(
        failures[0].context.direction,
        Some(lspf::ConnectionDirection::Inbound)
    );
    assert_eq!(
        failures[0].context.method.as_deref(),
        Some(ClientQuery::METHOD)
    );
    assert_eq!(
        failures[0].context.request_id,
        Some(ConnectionRequestId::Number(1))
    );
}

#[tokio::test]
async fn a_panicking_error_hook_cannot_suppress_a_protocol_response_or_cleanup() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&calls);
    let server = Server::builder(())
        .on_error(move |_| {
            observed_calls.fetch_add(1, Ordering::Relaxed);
            panic!("observer failed")
        })
        .build()
        .unwrap();
    let outbound = Arc::new(Mutex::new(Vec::new()));
    let transport = TestTransport {
        inbound: VecDeque::from([Ok(RawMessage::ProtocolError {
            error: JsonRpcError {
                code: -32700,
                message: "secret malformed payload".into(),
                data: None,
            },
        })]),
        outbound: Arc::clone(&outbound),
    };

    let outcome = server.serve(transport).await.unwrap();
    assert_eq!(outcome.code(), 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(matches!(
        outbound.lock().unwrap().as_slice(),
        [RawMessage::ProtocolError { .. }]
    ));
}

#[test]
fn only_one_error_hook_can_be_registered() {
    let result = Server::builder(())
        .on_error(|_| {})
        .on_error(|_| {})
        .build();
    assert!(matches!(result, Err(BuildError::DuplicateErrorHook)));
}

#[tokio::test]
async fn protocol_failure_reporting_does_not_enter_user_layers() {
    let layer_calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = Arc::new(AtomicUsize::new(0));
    let observed_hooks = Arc::clone(&hook_calls);
    let server = Server::builder(())
        .layer(CountingLayer(Arc::clone(&layer_calls)))
        .on_error(move |_| {
            observed_hooks.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();
    let transport = TestTransport {
        inbound: VecDeque::from([Ok(RawMessage::ProtocolError {
            error: JsonRpcError {
                code: -32700,
                message: "parse error".into(),
                data: None,
            },
        })]),
        outbound: Arc::new(Mutex::new(Vec::new())),
    };

    server.serve(transport).await.unwrap();
    assert_eq!(hook_calls.load(Ordering::Relaxed), 1);
    assert_eq!(layer_calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn engine_owned_protocol_rejections_each_report_once() {
    let cases = [
        VecDeque::from([Ok(request(2, Echo::METHOD, serde_json::Value::Null))]),
        VecDeque::from([
            Ok(initialize(1)),
            Ok(request(2, Echo::METHOD, serde_json::json!("first"))),
            Ok(request(2, Echo::METHOD, serde_json::json!("duplicate"))),
        ]),
        VecDeque::from([
            Ok(initialize(1)),
            Ok(notification(
                "$/cancelRequest",
                serde_json::json!("malformed"),
            )),
        ]),
        VecDeque::from([
            Ok(initialize(1)),
            Ok(notification("initialized", serde_json::json!(1))),
        ]),
        VecDeque::from([
            Ok(initialize(1)),
            Ok(RawMessage::Response {
                id: RequestId::Number(99),
                result: Ok(Bytes::from_static(b"null")),
            }),
        ]),
    ];

    for inbound in cases {
        let failures = Arc::new(Mutex::new(Vec::<ConnectionFailure>::new()));
        let recorded = Arc::clone(&failures);
        let server = Server::builder(())
            .request::<Echo, _, _>(|_, _, _, _| async { std::future::pending().await })
            .on_error(move |failure| recorded.lock().unwrap().push(failure))
            .build()
            .unwrap();
        server
            .serve(TestTransport {
                inbound,
                outbound: Arc::new(Mutex::new(Vec::new())),
            })
            .await
            .unwrap();

        let failures = failures.lock().unwrap();
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].category, ConnectionFailureCategory::Protocol);
    }
}

#[tokio::test]
async fn writer_send_and_shutdown_failures_have_distinct_categories() {
    for (failure, expected, inbound) in [
        (
            WriterFailure::Send,
            ConnectionFailureCategory::Transport,
            VecDeque::from([Ok(initialize(1))]),
        ),
        (
            WriterFailure::Shutdown,
            ConnectionFailureCategory::Close,
            VecDeque::from([
                Ok(initialize(1)),
                Ok(notification("exit", serde_json::Value::Null)),
            ]),
        ),
    ] {
        let failures = Arc::new(Mutex::new(Vec::<ConnectionFailure>::new()));
        let recorded = Arc::clone(&failures);
        let server = Server::builder(())
            .on_error(move |failure| recorded.lock().unwrap().push(failure))
            .build()
            .unwrap();

        let _ = server
            .serve(FailingWriterTransport { inbound, failure })
            .await;
        let failures = failures.lock().unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].category, expected);
        assert_eq!(
            failures[0].context.direction,
            Some(lspf::ConnectionDirection::Outbound)
        );
        assert!(!format!("{failures:?}").contains("secret"));
    }
}

#[tokio::test]
async fn framing_and_transport_reader_failures_have_distinct_categories() {
    for (transport_error, expected) in [
        (
            TransportError::Malformed("secret frame".into()),
            ConnectionFailureCategory::Framing,
        ),
        (
            TransportError::Io(std::io::Error::other("secret io")),
            ConnectionFailureCategory::Transport,
        ),
    ] {
        let failures = Arc::new(Mutex::new(Vec::<ConnectionFailure>::new()));
        let recorded = Arc::clone(&failures);
        let server = Server::builder(())
            .on_error(move |failure| recorded.lock().unwrap().push(failure))
            .build()
            .unwrap();
        let transport = TestTransport {
            inbound: VecDeque::from([Err(transport_error)]),
            outbound: Arc::new(Mutex::new(Vec::new())),
        };

        assert!(server.serve(transport).await.is_err());
        let failures = failures.lock().unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].category, expected);
        assert_eq!(
            failures[0].context.direction,
            Some(lspf::ConnectionDirection::Inbound)
        );
        assert!(failures[0].context.method.is_none());
        assert!(!format!("{failures:?}").contains("secret"));
    }
}

#[tokio::test]
async fn inbound_overload_reports_the_rejected_request_once() {
    let failures = Arc::new(Mutex::new(Vec::<ConnectionFailure>::new()));
    let recorded = Arc::clone(&failures);
    let server = Server::builder(())
        .request::<Echo, _, _>(|_, _, value, _| async move {
            if value == "hold" {
                std::future::pending().await
            } else {
                Ok(value)
            }
        })
        .resource_policy(ResourcePolicy {
            max_inbound_requests: 1,
            ..ResourcePolicy::default()
        })
        .on_error(move |failure| recorded.lock().unwrap().push(failure))
        .build()
        .unwrap();
    let transport = TestTransport {
        inbound: VecDeque::from([
            Ok(initialize(1)),
            Ok(request(2, Echo::METHOD, serde_json::json!("hold"))),
            Ok(request(3, Echo::METHOD, serde_json::json!("rejected"))),
            Ok(notification("exit", serde_json::Value::Null)),
        ]),
        outbound: Arc::new(Mutex::new(Vec::new())),
    };

    server.serve(transport).await.unwrap();
    let failures = failures.lock().unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].category, ConnectionFailureCategory::Overload);
    assert!(failures[0].context.method.is_none());
    assert_eq!(
        failures[0].context.request_id,
        Some(ConnectionRequestId::Number(3))
    );
}

#[tokio::test]
async fn outbound_overload_reports_the_rejected_response_once() {
    let failures = Arc::new(Mutex::new(Vec::<ConnectionFailure>::new()));
    let recorded = Arc::clone(&failures);
    let server = Server::builder(())
        .resource_policy(ResourcePolicy {
            max_outbound_messages: 1,
            ..ResourcePolicy::default()
        })
        .on_error(move |failure| recorded.lock().unwrap().push(failure))
        .build()
        .unwrap();
    let transport = SlowWriterTransport {
        inbound: VecDeque::from([
            Ok(initialize(1)),
            Ok(request(2, "test/missing", serde_json::Value::Null)),
        ]),
    };

    let outcome = server.serve(transport).await.unwrap();
    assert_eq!(outcome, lspf::Outcome::WriterFailed);
    let failures = failures.lock().unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].category, ConnectionFailureCategory::Overload);
    assert_eq!(
        failures[0].context.direction,
        Some(lspf::ConnectionDirection::Outbound)
    );
    assert_eq!(
        failures[0].context.request_id,
        Some(ConnectionRequestId::Number(2))
    );
}

#[tokio::test]
async fn document_overload_is_not_misreported_as_a_protocol_failure() {
    let failures = Arc::new(Mutex::new(Vec::<ConnectionFailure>::new()));
    let recorded = Arc::clone(&failures);
    let server = Server::builder(())
        .resource_policy(ResourcePolicy {
            max_documents: 1,
            ..ResourcePolicy::default()
        })
        .on_error(move |failure| recorded.lock().unwrap().push(failure))
        .build()
        .unwrap();
    let open = |uri: &str| {
        notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "text",
                    "version": 1,
                    "text": "contents"
                }
            }),
        )
    };
    let transport = TestTransport {
        inbound: VecDeque::from([
            Ok(initialize(1)),
            Ok(open("file:///one.txt")),
            Ok(open("file:///two.txt")),
            Ok(notification("exit", serde_json::Value::Null)),
        ]),
        outbound: Arc::new(Mutex::new(Vec::new())),
    };

    server.serve(transport).await.unwrap();
    let failures = failures.lock().unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].category, ConnectionFailureCategory::Overload);
    assert_eq!(
        failures[0].context.method.as_deref(),
        Some("textDocument/didOpen")
    );
}
