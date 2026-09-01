//! Adapter-neutral, wire-observed Transport conformance journey.
//!
//! First-party adapters provide only a wire client and a running real
//! [`Server`]. The journey never reaches into `ProtocolEngine` state.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gen_lsp_types::{InitializedParams, LogMessageParams, MessageType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::conformance_support::{self, LspError, Outcome, Server, ServerContext, TaskSend};
use crate::types::notification::Notification;
use crate::types::request::Request;

pub(crate) trait WireClient {
    fn send(&mut self, message: Value) -> impl Future<Output = ()> + TaskSend;
    fn receive(&mut self) -> impl Future<Output = Value> + TaskSend;
}

/// A `Content-Length`-framed wire client shared by the stdio and TCP adapter
/// tests: one end of the byte stream, split into read and write halves.
#[cfg(all(not(target_arch = "wasm32"), any(feature = "stdio", feature = "tcp")))]
pub(crate) struct ContentLengthClient<R, W> {
    pub(crate) reader: tokio_util::codec::FramedRead<R, conformance_support::ContentLengthCodec>,
    pub(crate) writer: W,
    codec: conformance_support::ContentLengthCodec,
}

#[cfg(all(not(target_arch = "wasm32"), any(feature = "stdio", feature = "tcp")))]
impl<R, W> ContentLengthClient<R, W>
where
    R: tokio::io::AsyncRead + Send + Unpin,
    W: tokio::io::AsyncWrite + Send + Unpin,
{
    pub(crate) fn new(reader: R, writer: W) -> Self {
        Self {
            reader: tokio_util::codec::FramedRead::new(
                reader,
                conformance_support::ContentLengthCodec::default(),
            ),
            writer,
            codec: conformance_support::ContentLengthCodec::default(),
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), any(feature = "stdio", feature = "tcp")))]
impl<R, W> WireClient for ContentLengthClient<R, W>
where
    R: tokio::io::AsyncRead + Send + Unpin,
    W: tokio::io::AsyncWrite + Send + Unpin,
{
    async fn send(&mut self, message: Value) {
        use tokio::io::AsyncWriteExt;
        use tokio_util::codec::Encoder;

        let body = serde_json::to_vec(&message).expect("the test message serializes");
        let mut frame = bytes::BytesMut::new();
        self.codec
            .encode(bytes::Bytes::from(body), &mut frame)
            .expect("the test message fits");
        self.writer
            .write_all(&frame)
            .await
            .expect("write test frame");
    }

    async fn receive(&mut self) -> Value {
        use futures_util::StreamExt;

        let body = self
            .reader
            .next()
            .await
            .expect("the server writes a frame")
            .expect("the server frame is well-formed");
        serde_json::from_slice(&body).expect("the server frame contains JSON")
    }
}

/// Establish an initialized session through the shared wire surface.
pub(crate) async fn initialize<C: WireClient>(client: &mut C) {
    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "processId": null, "rootUri": null, "capabilities": {} },
        }))
        .await;
    let response = client.receive().await;
    assert_eq!(response["id"], 1);
    client
        .send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }))
        .await;
    let observed = client.receive().await;
    assert_eq!(observed["method"], "window/logMessage");
    assert_eq!(observed["params"]["message"], "initialized observed");
}

#[derive(Debug, Deserialize, Serialize)]
struct ObserveParams {
    sequence: usize,
}

enum Observe {}

impl Notification for Observe {
    type Params = ObserveParams;
    const METHOD: &'static str = "conformance/observe";
}

#[derive(Debug, Deserialize, Serialize)]
struct JourneyParams {
    value: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct JourneyResult {
    echoed: String,
    observed_sequence: usize,
}

enum Journey {}

impl Request for Journey {
    type Params = JourneyParams;
    type Result = JourneyResult;
    const METHOD: &'static str = "conformance/journey";
}

enum EchoFromClient {}

impl Request for EchoFromClient {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "conformance/echoFromClient";
}

enum WaitForCancellation {}

impl Request for WaitForCancellation {
    type Params = Value;
    type Result = ();
    const METHOD: &'static str = "conformance/waitForCancellation";
}

enum WaitForSessionClose {}

impl Request for WaitForSessionClose {
    type Params = Value;
    type Result = ();
    const METHOD: &'static str = "conformance/waitForSessionClose";
}

pub(crate) struct ConformanceState {
    observed_sequence: AtomicUsize,
    task_drops: Arc<TaskDrops>,
}

#[derive(Default)]
struct TaskDrops {
    cancelled: Arc<AtomicUsize>,
    session_close: Arc<AtomicUsize>,
}

#[derive(Clone, Copy)]
enum TaskKind {
    Cancelled,
    SessionClose,
}

impl TaskKind {
    fn start(self, drops: &TaskDrops) -> (&'static str, TaskDropGuard) {
        match self {
            Self::Cancelled => (
                "cancellation handler started",
                TaskDropGuard(Arc::clone(&drops.cancelled)),
            ),
            Self::SessionClose => (
                "session-close handler started",
                TaskDropGuard(Arc::clone(&drops.session_close)),
            ),
        }
    }
}

struct TaskDropGuard(Arc<AtomicUsize>);

impl Drop for TaskDropGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) struct TaskProbe(Arc<TaskDrops>);

#[cfg(target_arch = "wasm32")]
impl TaskProbe {
    pub(crate) fn cancelled_task_dropped(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst) == 1
    }

    pub(crate) fn session_close_task_dropped(&self) -> bool {
        self.0.session_close.load(Ordering::SeqCst) == 1
    }
}

async fn pending_task(
    ctx: ServerContext,
    drops: Arc<TaskDrops>,
    kind: TaskKind,
) -> Result<(), LspError> {
    let (started_message, _drop_guard) = kind.start(&drops);
    ctx.client()
        .log_message(LogMessageParams {
            kind: MessageType::Info,
            message: started_message.to_string(),
        })
        .map_err(|error| LspError::internal(error.to_string()))?;
    std::future::pending().await
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn server() -> Server<ConformanceState> {
    build_server().0
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn server_with_task_probe() -> (Server<ConformanceState>, TaskProbe) {
    let (server, task_drops) = build_server();
    (server, TaskProbe(task_drops))
}

fn build_server() -> (Server<ConformanceState>, Arc<TaskDrops>) {
    let task_drops = Arc::new(TaskDrops::default());
    let state = ConformanceState {
        observed_sequence: AtomicUsize::new(0),
        task_drops: Arc::clone(&task_drops),
    };
    #[cfg(target_arch = "wasm32")]
    let non_send_value = std::rc::Rc::new(wasm_bindgen::JsValue::from_str("héllo"));
    let server = Server::builder(state)
        .on_initialized(
            |_state: Arc<ConformanceState>, ctx: ServerContext, _params: InitializedParams| async move {
                ctx.client()
                    .log_message(LogMessageParams {
                        kind: MessageType::Info,
                        message: "initialized observed".to_string(),
                    })
                    .expect("the initialized connection is open");
            },
        )
        .notification::<Observe, _, _>(
            |state: Arc<ConformanceState>, ctx: ServerContext, params: ObserveParams| async move {
                state
                    .observed_sequence
                    .store(params.sequence, Ordering::SeqCst);
                ctx.client()
                    .log_message(LogMessageParams {
                        kind: MessageType::Info,
                        message: "conformance observed".to_string(),
                    })
                    .expect("the conformance connection is open");
            },
        )
        .request::<Journey, _, _>(
            move |state: Arc<ConformanceState>, ctx: ServerContext, params: JourneyParams, _ct| {
                #[cfg(target_arch = "wasm32")]
                let non_send_value = std::rc::Rc::clone(&non_send_value);
                async move {
                    let echoed = ctx
                        .client()
                        .request::<EchoFromClient>(params.value)
                        .await
                        .map_err(|error| LspError::internal(error.to_string()))?;
                    #[cfg(target_arch = "wasm32")]
                    assert_eq!(non_send_value.as_string().as_deref(), Some("héllo"));
                    ctx.client()
                        .log_message(LogMessageParams {
                            kind: MessageType::Info,
                            message: "conformance notification".to_string(),
                        })
                        .map_err(|error| LspError::internal(error.to_string()))?;
                    Ok(JourneyResult {
                        echoed,
                        observed_sequence: state.observed_sequence.load(Ordering::SeqCst),
                    })
                }
            },
        )
        .request::<WaitForCancellation, _, _>(
            |state: Arc<ConformanceState>, ctx, _params: Value, _cancellation| {
                pending_task(ctx, Arc::clone(&state.task_drops), TaskKind::Cancelled)
            },
        )
        .request::<WaitForSessionClose, _, _>(
            |state: Arc<ConformanceState>, ctx, _params: Value, _cancellation| {
                pending_task(ctx, Arc::clone(&state.task_drops), TaskKind::SessionClose)
            },
        )
        .build()
        .expect("the conformance Server builds");
    (server, task_drops)
}

/// Run the single journey shared by every first-party Transport adapter.
pub(crate) async fn run<C, F>(client: &mut C, serving: F)
where
    C: WireClient,
    F: Future<Output = conformance_support::Result<Outcome>>,
{
    let journey = async {
        initialize(client).await;
        client
            .send(json!({
                "jsonrpc": "2.0",
                "method": "conformance/observe",
                "params": { "sequence": 7 },
            }))
            .await;
        let observed = client.receive().await;
        assert_eq!(observed["method"], "window/logMessage");
        assert_eq!(observed["params"]["message"], "conformance observed");
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "conformance/journey",
                "params": { "value": "héllo" },
            }))
            .await;

        let outbound_request = client.receive().await;
        assert_eq!(outbound_request["method"], "conformance/echoFromClient");
        assert_eq!(outbound_request["params"], "héllo");
        let outbound_id = outbound_request["id"].clone();
        client
            .send(json!({ "jsonrpc": "2.0", "id": outbound_id, "result": "echoed" }))
            .await;

        let notification = client.receive().await;
        assert_eq!(notification["method"], "window/logMessage");
        assert_eq!(
            notification["params"]["message"],
            "conformance notification"
        );
        let journey = client.receive().await;
        assert_eq!(journey["id"], 2);
        assert_eq!(
            journey["result"],
            json!({ "echoed": "echoed", "observed_sequence": 7 })
        );

        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "conformance/waitForCancellation",
                "params": {},
            }))
            .await;
        let cancellation_started = client.receive().await;
        assert_eq!(cancellation_started["method"], "window/logMessage");
        assert_eq!(
            cancellation_started["params"]["message"],
            "cancellation handler started"
        );
        client
            .send(json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": { "id": 3 },
            }))
            .await;
        let cancelled = client.receive().await;
        assert_eq!(cancelled["id"], 3);
        assert_eq!(cancelled["error"]["code"], -32800);

        client
            .send(json!({ "jsonrpc": "2.0", "id": 4, "method": "shutdown" }))
            .await;
        let shutdown = client.receive().await;
        assert_eq!(shutdown["id"], 4);
        assert_eq!(shutdown["result"], Value::Null);
        client
            .send(json!({ "jsonrpc": "2.0", "method": "exit" }))
            .await;
    };

    let ((), outcome) = futures_util::join!(journey, serving);
    let outcome = outcome.expect("the conformance journey serves without a transport error");
    assert_eq!(outcome, Outcome::Exit { code: 0 });
}
