//! Public-boundary coverage for the fixed Service stack (issue #44).

use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use lspf::types::notification::Notification;
use lspf::types::request::Request;
use lspf::{
    CallKind, Context, IncomingCall, Layer, Next, RawMessage, RequestId, Server, ServiceFuture,
    ServiceResult, Transport, TransportError, TransportReader, TransportWriter,
};
use serde_json::json;
use tokio::sync::{Semaphore, mpsc};

enum Echo {}

impl Request for Echo {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "test/echo";
}

struct AppState {
    events: Arc<Mutex<Vec<&'static str>>>,
    notification_done: Arc<tokio::sync::Notify>,
}

async fn echo(
    state: Arc<AppState>,
    _ctx: Context,
    value: String,
    _cancellation: lspf::CancellationToken,
) -> Result<String, lspf::LspError> {
    state.events.lock().unwrap().push("router");
    Ok(value)
}

enum TestNotification {}

impl Notification for TestNotification {
    type Params = String;
    const METHOD: &'static str = "test/notification";
}

async fn sometimes_panics(
    _state: Arc<AppState>,
    _ctx: Context,
    value: String,
    _cancellation: lspf::CancellationToken,
) -> Result<String, lspf::LspError> {
    assert_ne!(value, "handler panic");
    Ok(value)
}

async fn notification_sometimes_panics(state: Arc<AppState>, _ctx: Context, value: String) {
    assert_ne!(value, "notification panic");
    state.events.lock().unwrap().push("later notification");
    state.notification_done.notify_one();
}

struct PanickingLayer;

impl Layer<AppState> for PanickingLayer {
    fn call(&self, call: IncomingCall<AppState>, next: Next<AppState>) -> ServiceFuture {
        assert_ne!(call.params(), &json!("layer panic"));
        next.call(call)
    }
}

struct RecordingLayer {
    enter: &'static str,
    exit: &'static str,
    inspect: bool,
}

impl Layer<AppState> for RecordingLayer {
    fn call(&self, call: IncomingCall<AppState>, next: Next<AppState>) -> ServiceFuture {
        let _another_handle_to_the_same_inner_service = next.clone();
        if self.inspect {
            assert_eq!(call.kind(), CallKind::Request);
            assert_eq!(call.method(), Echo::METHOD);
            assert_eq!(call.request_id(), Some(&RequestId::Number(2)));
            assert_eq!(call.params(), &json!("hello"));
            assert_eq!(call.context().request_id(), call.request_id());
        }
        let events = Arc::clone(&call.state().events);
        let enter = self.enter;
        let exit = self.exit;
        Box::pin(async move {
            events.lock().unwrap().push(enter);
            let result = next.call(call).await;
            events.lock().unwrap().push(exit);
            result
        })
    }
}

struct ChannelTransport {
    incoming: mpsc::UnboundedReceiver<RawMessage>,
    outgoing: mpsc::UnboundedSender<RawMessage>,
}

struct ChannelReader(mpsc::UnboundedReceiver<RawMessage>);
struct ChannelWriter(mpsc::UnboundedSender<RawMessage>);

#[derive(Clone, Default)]
struct SpanCapture {
    closed: Arc<Mutex<Vec<(String, Duration)>>>,
}

struct OpenedAt(Instant);

impl<S> tracing_subscriber::Layer<S> for SpanCapture
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        _attrs: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if let Some(span) = context.span(id) {
            span.extensions_mut().insert(OpenedAt(Instant::now()));
        }
    }

    fn on_close(&self, id: tracing::Id, context: tracing_subscriber::layer::Context<'_, S>) {
        let Some(span) = context.span(&id) else {
            return;
        };
        let elapsed = span
            .extensions()
            .get::<OpenedAt>()
            .map(|opened| opened.0.elapsed())
            .unwrap_or_default();
        self.closed
            .lock()
            .unwrap()
            .push((span.metadata().name().to_string(), elapsed));
    }
}

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

fn notification(method: &'static str) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from_static(b"null"),
    }
}

async fn receive_for(
    outgoing: &mut mpsc::UnboundedReceiver<RawMessage>,
    expected_id: i32,
) -> RawMessage {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let message = outgoing.recv().await.expect("server response");
            if message.id() == Some(&RequestId::Number(expected_id)) {
                return message;
            }
        }
    })
    .await
    .expect("server response before watchdog timeout")
}

fn start<S: Send + Sync + 'static>(
    server: Server<S>,
) -> (
    mpsc::UnboundedSender<RawMessage>,
    mpsc::UnboundedReceiver<RawMessage>,
    tokio::task::JoinHandle<lspf::Result<lspf::Outcome>>,
) {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, outgoing_rx) = mpsc::unbounded_channel();
    let serve = tokio::spawn(server.serve(ChannelTransport {
        incoming: incoming_rx,
        outgoing: outgoing_tx,
    }));
    (incoming_tx, outgoing_rx, serve)
}

async fn initialize_connection(
    incoming: &mpsc::UnboundedSender<RawMessage>,
    outgoing: &mut mpsc::UnboundedReceiver<RawMessage>,
) {
    incoming
        .send(request(
            1,
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        ))
        .unwrap();
    let _ = receive_for(outgoing, 1).await;
}

async fn stop(
    incoming: mpsc::UnboundedSender<RawMessage>,
    serve: tokio::task::JoinHandle<lspf::Result<lspf::Outcome>>,
) {
    incoming.send(notification("exit")).unwrap();
    serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incoming_call_and_last_registered_outermost_order_are_preserved() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let server = Server::builder(AppState {
        events: Arc::clone(&events),
        notification_done: Arc::new(tokio::sync::Notify::new()),
    })
    .request::<Echo, _, _>(echo)
    .layer(RecordingLayer {
        enter: "first in",
        exit: "first out",
        inspect: false,
    })
    .layer(RecordingLayer {
        enter: "second in",
        exit: "second out",
        inspect: true,
    })
    .build()
    .expect("server builds");
    let (incoming_tx, mut outgoing_rx, serve) = start(server);
    initialize_connection(&incoming_tx, &mut outgoing_rx).await;
    incoming_tx
        .send(request(2, Echo::METHOD, json!("hello")))
        .unwrap();

    assert!(matches!(
        receive_for(&mut outgoing_rx, 2).await,
        RawMessage::Response {
            result: Ok(ref value),
            ..
        } if serde_json::from_slice::<String>(value).unwrap() == "hello"
    ));
    assert_eq!(
        *events.lock().unwrap(),
        ["second in", "first in", "router", "first out", "second out"]
    );

    stop(incoming_tx, serve).await;
}

fn error_code(message: &RawMessage) -> Option<i32> {
    match message {
        RawMessage::Response {
            result: Err(error), ..
        } => Some(error.code),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panics_are_isolated_and_the_connection_processes_later_calls() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let notification_done = Arc::new(tokio::sync::Notify::new());
    let server = Server::builder(AppState {
        events: Arc::clone(&events),
        notification_done: Arc::clone(&notification_done),
    })
    .request::<Echo, _, _>(sometimes_panics)
    .notification::<TestNotification, _, _>(notification_sometimes_panics)
    .layer(PanickingLayer)
    .build()
    .expect("server builds");
    let (incoming_tx, mut outgoing_rx, serve) = start(server);
    initialize_connection(&incoming_tx, &mut outgoing_rx).await;

    incoming_tx
        .send(request(2, Echo::METHOD, json!("handler panic")))
        .unwrap();
    assert_eq!(
        error_code(&receive_for(&mut outgoing_rx, 2).await),
        Some(-32603)
    );

    incoming_tx
        .send(request(3, Echo::METHOD, json!("layer panic")))
        .unwrap();
    assert_eq!(
        error_code(&receive_for(&mut outgoing_rx, 3).await),
        Some(-32603)
    );

    incoming_tx
        .send(request(4, Echo::METHOD, json!("later request")))
        .unwrap();
    assert!(matches!(
        receive_for(&mut outgoing_rx, 4).await,
        RawMessage::Response { result: Ok(ref value), .. }
            if serde_json::from_slice::<String>(value).unwrap() == "later request"
    ));

    incoming_tx
        .send(RawMessage::Notification {
            method: Cow::Borrowed(TestNotification::METHOD),
            params: Bytes::from(serde_json::to_vec(&json!("notification panic")).unwrap()),
        })
        .unwrap();
    incoming_tx
        .send(RawMessage::Notification {
            method: Cow::Borrowed(TestNotification::METHOD),
            params: Bytes::from(serde_json::to_vec(&json!("later notification")).unwrap()),
        })
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        notification_done.notified(),
    )
    .await
    .expect("later notification ran after panic");
    assert_eq!(*events.lock().unwrap(), ["later notification"]);
    assert!(
        outgoing_rx.try_recv().is_err(),
        "notifications must not emit responses"
    );

    stop(incoming_tx, serve).await;
}

enum Slow {}

impl Request for Slow {
    type Params = usize;
    type Result = usize;
    const METHOD: &'static str = "test/slow";
}

struct ConcurrencyState;

async fn slow(
    _state: Arc<ConcurrencyState>,
    _ctx: Context,
    value: usize,
    _cancellation: lspf::CancellationToken,
) -> Result<usize, lspf::LspError> {
    Ok(value)
}

struct BlockingLayer {
    entered: mpsc::UnboundedSender<usize>,
    release: Arc<Semaphore>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

impl Layer<ConcurrencyState> for BlockingLayer {
    fn call(
        &self,
        call: IncomingCall<ConcurrencyState>,
        next: Next<ConcurrencyState>,
    ) -> ServiceFuture {
        let value = call.params().as_u64().unwrap() as usize;
        let entered = self.entered.clone();
        let release = Arc::clone(&self.release);
        let active = Arc::clone(&self.active);
        let max_active = Arc::clone(&self.max_active);
        Box::pin(async move {
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            max_active.fetch_max(now, Ordering::SeqCst);
            entered.send(value).unwrap();
            let permit = release.acquire_owned().await.unwrap();
            permit.forget();
            let result = next.call(call).await;
            active.fetch_sub(1, Ordering::SeqCst);
            result
        })
    }
}

#[test]
fn zero_concurrency_limit_is_rejected() {
    let result = Server::builder(ConcurrencyState)
        .concurrency_limit(0)
        .build();
    assert!(matches!(
        result,
        Err(lspf::BuildError::InvalidConcurrencyLimit)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrency_limit_covers_the_complete_user_layer_chain() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let capture = SpanCapture::default();
    tracing_subscriber::registry()
        .with(capture.clone())
        .try_init()
        .expect("this integration-test process installs one subscriber");
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let server = Server::builder(ConcurrencyState)
        .request::<Slow, _, _>(slow)
        .layer(BlockingLayer {
            entered: entered_tx,
            release: Arc::clone(&release),
            active: Arc::clone(&active),
            max_active: Arc::clone(&max_active),
        })
        .concurrency_limit(1)
        .build()
        .expect("server builds");
    let (incoming_tx, mut outgoing_rx, serve) = start(server);
    initialize_connection(&incoming_tx, &mut outgoing_rx).await;
    incoming_tx
        .send(request(2, Slow::METHOD, json!(1)))
        .unwrap();
    incoming_tx
        .send(request(3, Slow::METHOD, json!(2)))
        .unwrap();

    let first = entered_rx.recv().await.expect("one Layer call entered");
    const QUEUE_HOLD: Duration = Duration::from_millis(80);
    tokio::time::sleep(QUEUE_HOLD).await;
    release.add_permits(1);
    let _ = receive_for(&mut outgoing_rx, first as i32 + 1).await;
    let second = entered_rx
        .recv()
        .await
        .expect("the queued Layer call entered");
    assert_ne!(first, second);
    release.add_permits(1);
    let _ = receive_for(&mut outgoing_rx, second as i32 + 1).await;
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
    let max_queue = capture
        .closed
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == "handler.acquire_permit")
        .map(|(_, elapsed)| *elapsed)
        .max()
        .unwrap_or_default();
    assert!(
        max_queue >= QUEUE_HOLD / 2,
        "the acquire span must include permit queue time; longest was {max_queue:?}"
    );

    stop(incoming_tx, serve).await;
}

static PARAM_DECODES: AtomicUsize = AtomicUsize::new(0);
static RESULT_ENCODES: AtomicUsize = AtomicUsize::new(0);

struct CountedParams(String);

impl serde::Serialize for CountedParams {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&self.0, serializer)
    }
}

impl<'de> serde::Deserialize<'de> for CountedParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        PARAM_DECODES.fetch_add(1, Ordering::SeqCst);
        serde::Deserialize::deserialize(deserializer).map(Self)
    }
}

struct CountedResult(String);

impl serde::Serialize for CountedResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        RESULT_ENCODES.fetch_add(1, Ordering::SeqCst);
        serde::Serialize::serialize(&self.0, serializer)
    }
}

impl<'de> serde::Deserialize<'de> for CountedResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde::Deserialize::deserialize(deserializer).map(Self)
    }
}

enum Counted {}

impl Request for Counted {
    type Params = CountedParams;
    type Result = CountedResult;
    const METHOD: &'static str = "test/counted";
}

async fn counted(
    _state: Arc<ConcurrencyState>,
    _ctx: Context,
    params: CountedParams,
    _cancellation: lspf::CancellationToken,
) -> Result<CountedResult, lspf::LspError> {
    Ok(CountedResult(params.0))
}

struct PassThroughLayer;

impl Layer<ConcurrencyState> for PassThroughLayer {
    fn call(
        &self,
        call: IncomingCall<ConcurrencyState>,
        next: Next<ConcurrencyState>,
    ) -> ServiceFuture {
        Box::pin(async move {
            match next.call(call).await {
                ServiceResult::Response(mut value) => {
                    assert!(value.is_string());
                    ServiceResult::Response(value.take())
                }
                result => result,
            }
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn values_cross_layers_with_one_typed_decode_and_encode() {
    PARAM_DECODES.store(0, Ordering::SeqCst);
    RESULT_ENCODES.store(0, Ordering::SeqCst);
    let server = Server::builder(ConcurrencyState)
        .request::<Counted, _, _>(counted)
        .layer(PassThroughLayer)
        .layer(PassThroughLayer)
        .build()
        .expect("server builds");
    let (incoming_tx, mut outgoing_rx, serve) = start(server);
    initialize_connection(&incoming_tx, &mut outgoing_rx).await;
    incoming_tx
        .send(request(2, Counted::METHOD, json!("value")))
        .unwrap();
    assert!(matches!(
        receive_for(&mut outgoing_rx, 2).await,
        RawMessage::Response { result: Ok(ref value), .. }
            if serde_json::from_slice::<String>(value).unwrap() == "value"
    ));
    assert_eq!(PARAM_DECODES.load(Ordering::SeqCst), 1);
    assert_eq!(RESULT_ENCODES.load(Ordering::SeqCst), 1);

    stop(incoming_tx, serve).await;
}
