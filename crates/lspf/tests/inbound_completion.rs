//! Exactly-once inbound request completion (issue #43).
//!
//! These tests drive the public `Server::serve` transport seam. Channels and
//! notifications establish the relevant ordering; no assertion depends on a
//! scheduler delay.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use lspf::types::request::Request;
use lspf::{
    IncomingCall, Layer, Next, RawMessage, RequestId, ResourcePolicy, Server, ServerContext,
    ServiceFuture, Transport, TransportError, TransportReader, TransportWriter,
};
use serde_json::json;
use tokio::sync::{Mutex, Notify, mpsc, oneshot};

enum Controlled {}

impl Request for Controlled {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "test/controlled";
}

enum ReturnsAfterCancellation {}

impl Request for ReturnsAfterCancellation {
    type Params = ();
    type Result = String;
    const METHOD: &'static str = "test/returns-after-cancellation";
}

enum PanicsAfterCancellation {}

impl Request for PanicsAfterCancellation {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "test/panics-after-cancellation";
}

struct AppState {
    calls: Arc<AtomicUsize>,
    started: Mutex<Option<oneshot::Sender<()>>>,
    cancellation_observed: Mutex<Option<oneshot::Sender<()>>>,
    release: Arc<Notify>,
}

struct HandlerTimeout(std::time::Duration);

impl Layer<AppState> for HandlerTimeout {
    fn call(&self, mut call: IncomingCall<AppState>, next: Next<AppState>) -> ServiceFuture {
        call.set_handler_timeout(self.0);
        next.call(call)
    }
}

async fn controlled(
    state: Arc<AppState>,
    _ctx: ServerContext,
    value: String,
    ct: lspf::CancellationToken,
) -> Result<String, lspf::LspError> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    // Register for the release notification before signalling `started`. A
    // `notify_one` that lands between `started.send` and the select's first
    // poll finds no registered waiter and collapses into the Notify's single
    // stored permit; a second `notify_one` is then a no-op, so with two
    // handlers racing on the same Notify one of them would starve. `enable`
    // puts this waiter in the list up front, so the first notification is
    // always assigned to a waiter and the second always stores a fresh permit.
    let notified = state.release.notified();
    tokio::pin!(notified);
    let _ = notified.as_mut().enable();
    if let Some(started) = state.started.lock().await.take() {
        let _ = started.send(());
    }
    tokio::select! {
        _ = notified => Ok(value),
        _ = ct.cancelled() => {
            if let Some(observed) = state.cancellation_observed.lock().await.take() {
                let _ = observed.send(());
            }
            std::future::pending().await
        }
    }
}

async fn returns_after_cancellation(
    state: Arc<AppState>,
    _ctx: ServerContext,
    (): (),
    ct: lspf::CancellationToken,
) -> Result<String, lspf::LspError> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    if let Some(started) = state.started.lock().await.take() {
        let _ = started.send(());
    }
    ct.cancelled().await;
    if let Some(observed) = state.cancellation_observed.lock().await.take() {
        let _ = observed.send(());
    }
    Ok("too late".to_string())
}

async fn panics_after_cancellation(
    state: Arc<AppState>,
    _ctx: ServerContext,
    (): (),
    ct: lspf::CancellationToken,
) -> Result<(), lspf::LspError> {
    if let Some(started) = state.started.lock().await.take() {
        let _ = started.send(());
    }
    ct.cancelled().await;
    if let Some(observed) = state.cancellation_observed.lock().await.take() {
        let _ = observed.send(());
    }
    panic!("panic racing handler expiry");
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

fn notification(method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

/// Hang guard for every wait in this file. Assertions never depend on a
/// scheduler delay; the watchdog only bounds a genuinely stuck engine, so it
/// stays generous enough for instrumented (llvm-cov) CI runs.
const WATCHDOG: std::time::Duration = std::time::Duration::from_secs(30);

async fn receive_for(
    outgoing: &mut mpsc::UnboundedReceiver<RawMessage>,
    expected_id: i32,
) -> RawMessage {
    tokio::time::timeout(WATCHDOG, async {
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

async fn assert_handler_deadline_expired(
    outgoing: &mut mpsc::UnboundedReceiver<RawMessage>,
    id: i32,
) {
    assert!(matches!(
        receive_for(outgoing, id).await,
        RawMessage::Response {
            result: Err(ref error),
            ..
        } if error.code == -32802 && error.message == "handler deadline expired"
    ));
}

type Serving = tokio::task::JoinHandle<lspf::Result<lspf::Outcome>>;

fn one_slot_policy() -> ResourcePolicy {
    ResourcePolicy {
        max_inbound_requests: 1,
        ..ResourcePolicy::default()
    }
}

fn cancellation_observed_state() -> (AppState, oneshot::Receiver<()>, oneshot::Receiver<()>) {
    let (started_tx, started_rx) = oneshot::channel();
    let (observed_tx, observed_rx) = oneshot::channel();
    (
        AppState {
            calls: Arc::new(AtomicUsize::new(0)),
            started: Mutex::new(Some(started_tx)),
            cancellation_observed: Mutex::new(Some(observed_tx)),
            release: Arc::new(Notify::new()),
        },
        started_rx,
        observed_rx,
    )
}

async fn start_initialized(
    server: Server<AppState>,
) -> (
    mpsc::UnboundedSender<RawMessage>,
    mpsc::UnboundedReceiver<RawMessage>,
    Serving,
) {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
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
    let _ = receive_for(&mut outgoing_rx, 1).await;
    (incoming_tx, outgoing_rx, serve)
}

async fn stop_and_assert_no_more_responses(
    incoming: &mpsc::UnboundedSender<RawMessage>,
    outgoing: &mut mpsc::UnboundedReceiver<RawMessage>,
    serve: Serving,
    answered_id: i32,
) {
    incoming.send(notification("exit", json!(null))).unwrap();
    serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
    while let Ok(message) = outgoing.try_recv() {
        assert_ne!(
            message.id(),
            Some(&RequestId::Number(answered_id)),
            "a completed request emitted an additional response"
        );
    }
}

async fn cancel_first_request_admitted_after_reap(
    incoming: &mpsc::UnboundedSender<RawMessage>,
    outgoing: &mut mpsc::UnboundedReceiver<RawMessage>,
    ids: std::ops::RangeInclusive<i32>,
    method: &'static str,
    params: serde_json::Value,
) -> i32 {
    for id in ids {
        incoming.send(request(id, method, params.clone())).unwrap();
        incoming
            .send(notification("$/cancelRequest", json!({ "id": id })))
            .unwrap();
        match receive_for(outgoing, id).await {
            RawMessage::Response {
                result: Err(error), ..
            } if error.code == -32802 => continue,
            RawMessage::Response {
                result: Err(error), ..
            } if error.code == -32800 => return id,
            other => panic!("expected overload or admitted cancellation, got {other:?}"),
        }
    }
    panic!("a finished task was not reaped within the retry budget");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_in_flight_id_is_rejected_without_replacing_original_request() {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
    let (started_tx, started_rx) = oneshot::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Notify::new());
    let state = AppState {
        calls: Arc::clone(&calls),
        started: Mutex::new(Some(started_tx)),
        cancellation_observed: Mutex::new(None),
        release: Arc::clone(&release),
    };
    let server = Server::builder(state)
        .request::<Controlled, _, _>(controlled)
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
    assert!(matches!(
        receive_for(&mut outgoing_rx, 1).await,
        RawMessage::Response { result: Ok(_), .. }
    ));

    incoming_tx
        .send(request(2, Controlled::METHOD, json!("original")))
        .unwrap();
    started_rx.await.expect("original handler started");

    incoming_tx
        .send(request(2, Controlled::METHOD, json!("duplicate")))
        .unwrap();
    assert!(matches!(
        receive_for(&mut outgoing_rx, 2).await,
        RawMessage::Response {
            result: Err(ref error),
            ..
        } if error.code == -32600
    ));

    release.notify_one();
    assert!(matches!(
        receive_for(&mut outgoing_rx, 2).await,
        RawMessage::Response {
            result: Ok(ref result),
            ..
        } if serde_json::from_slice::<String>(result).unwrap() == "original"
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    incoming_tx.send(notification("exit", json!(null))).unwrap();
    serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn excess_requests_are_rejected_before_dispatch_and_completion_releases_capacity() {
    let (started_tx, started_rx) = oneshot::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Notify::new());
    let state = AppState {
        calls: Arc::clone(&calls),
        started: Mutex::new(Some(started_tx)),
        cancellation_observed: Mutex::new(None),
        release: Arc::clone(&release),
    };
    let server = Server::builder(state)
        .request::<Controlled, _, _>(controlled)
        .resource_policy(one_slot_policy())
        .build()
        .expect("server builds");
    let (incoming_tx, mut outgoing_rx, serve) = start_initialized(server).await;

    incoming_tx
        .send(request(2, Controlled::METHOD, json!("accepted")))
        .unwrap();
    started_rx.await.expect("accepted handler started");
    incoming_tx
        .send(request(2, Controlled::METHOD, json!("duplicate")))
        .unwrap();
    assert!(matches!(
        receive_for(&mut outgoing_rx, 2).await,
        RawMessage::Response {
            result: Err(ref error),
            ..
        } if error.code == -32600 && error.message == "duplicate request id"
    ));

    for id in 3..=66 {
        incoming_tx
            .send(request(id, Controlled::METHOD, json!("excess")))
            .unwrap();
    }
    for id in 3..=66 {
        let excess = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            receive_for(&mut outgoing_rx, id),
        )
        .await
        .expect("excess request is rejected without waiting for handler capacity");
        assert!(matches!(
            excess,
            RawMessage::Response {
                result: Err(ref error),
                ..
            } if error.code == -32802
                && error.message == "inbound request capacity exhausted"
        ));
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "excess work was not dispatched"
    );

    release.notify_one();
    assert!(matches!(
        receive_for(&mut outgoing_rx, 2).await,
        RawMessage::Response { result: Ok(_), .. }
    ));

    let admitted = cancel_first_request_admitted_after_reap(
        &incoming_tx,
        &mut outgoing_rx,
        67..=74,
        Controlled::METHOD,
        json!("after completion"),
    )
    .await;
    assert!(
        admitted >= 67,
        "completion released capacity after task reap"
    );
    assert!(calls.load(Ordering::SeqCst) <= 2);

    incoming_tx.send(notification("exit", json!(null))).unwrap();
    serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_reaches_handler_token_and_completes_unfinished_request_once() {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
    let (started_tx, started_rx) = oneshot::channel();
    let (observed_tx, observed_rx) = oneshot::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let state = AppState {
        calls: Arc::clone(&calls),
        started: Mutex::new(Some(started_tx)),
        cancellation_observed: Mutex::new(Some(observed_tx)),
        release: Arc::new(Notify::new()),
    };
    let server = Server::builder(state)
        .request::<Controlled, _, _>(controlled)
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
    let _ = receive_for(&mut outgoing_rx, 1).await;

    incoming_tx
        .send(request(2, Controlled::METHOD, json!("unfinished")))
        .unwrap();
    started_rx.await.expect("handler started");
    incoming_tx
        .send(notification("$/cancelRequest", json!({ "id": 2 })))
        .unwrap();

    tokio::time::timeout(WATCHDOG, observed_rx)
        .await
        .expect("handler observed cancellation before watchdog timeout")
        .expect("handler observed its cancellation token");
    let response = receive_for(&mut outgoing_rx, 2).await;
    assert!(matches!(
        response,
        RawMessage::Response {
            result: Err(ref error),
            ..
        } if error.code == -32800
    ));

    incoming_tx.send(notification("exit", json!(null))).unwrap();
    serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
    while let Ok(message) = outgoing_rx.try_recv() {
        assert_ne!(
            message.id(),
            Some(&RequestId::Number(2)),
            "the cancelled request emitted more than one response"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn layer_override_expires_with_one_stable_response_and_cooperative_cancellation() {
    let (state, started_rx, mut observed_rx) = cancellation_observed_state();
    let server = Server::builder(state)
        .request::<Controlled, _, _>(controlled)
        .layer(HandlerTimeout(std::time::Duration::from_secs(5)))
        .build()
        .expect("server builds");
    let (incoming_tx, mut outgoing_rx, serve) = start_initialized(server).await;
    incoming_tx
        .send(request(2, Controlled::METHOD, json!("unfinished")))
        .unwrap();
    started_rx.await.expect("handler started");

    tokio::time::advance(std::time::Duration::from_secs(4)).await;
    tokio::task::yield_now().await;
    assert!(outgoing_rx.try_recv().is_err(), "deadline has not elapsed");
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    assert!(
        matches!(observed_rx.try_recv(), Ok(())),
        "the Layer override expires at five seconds"
    );
    assert_handler_deadline_expired(&mut outgoing_rx, 2).await;
    stop_and_assert_no_more_responses(&incoming_tx, &mut outgoing_rx, serve, 2).await;
}

#[tokio::test(start_paused = true)]
async fn default_handler_timeout_is_finite_and_enforced() {
    let (state, started_rx, observed_rx) = cancellation_observed_state();
    let server = Server::builder(state)
        .request::<Controlled, _, _>(controlled)
        .build()
        .expect("server builds");
    let (incoming_tx, mut outgoing_rx, serve) = start_initialized(server).await;
    incoming_tx
        .send(request(2, Controlled::METHOD, json!("unfinished")))
        .unwrap();
    started_rx.await.expect("handler started");

    tokio::time::advance(std::time::Duration::from_secs(29)).await;
    tokio::task::yield_now().await;
    assert!(outgoing_rx.try_recv().is_err(), "default deadline is 30s");
    tokio::time::advance(std::time::Duration::from_secs(1)).await;

    observed_rx
        .await
        .expect("handler observed default deadline cancellation");
    assert_handler_deadline_expired(&mut outgoing_rx, 2).await;
    stop_and_assert_no_more_responses(&incoming_tx, &mut outgoing_rx, serve, 2).await;
}

#[tokio::test(start_paused = true)]
async fn timeout_wins_when_cooperative_handler_returns_success_and_releases_the_id() {
    let (state, started_rx, observed_rx) = cancellation_observed_state();
    let server = Server::builder(state)
        .request::<ReturnsAfterCancellation, _, _>(returns_after_cancellation)
        .layer(HandlerTimeout(std::time::Duration::from_secs(5)))
        .build()
        .expect("server builds");
    let (incoming_tx, mut outgoing_rx, serve) = start_initialized(server).await;
    incoming_tx
        .send(request(2, ReturnsAfterCancellation::METHOD, json!(null)))
        .unwrap();
    started_rx.await.expect("handler started");
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    observed_rx.await.expect("handler observed timeout");

    assert_handler_deadline_expired(&mut outgoing_rx, 2).await;

    incoming_tx
        .send(request(2, ReturnsAfterCancellation::METHOD, json!(null)))
        .unwrap();
    incoming_tx
        .send(notification("$/cancelRequest", json!({ "id": 2 })))
        .unwrap();
    assert!(matches!(
        receive_for(&mut outgoing_rx, 2).await,
        RawMessage::Response {
            result: Err(ref error),
            ..
        } if error.code == -32800
    ));

    stop_and_assert_no_more_responses(&incoming_tx, &mut outgoing_rx, serve, 2).await;
}

#[tokio::test(start_paused = true)]
async fn timeout_and_panic_race_emits_only_the_timeout_response() {
    let (state, started_rx, observed_rx) = cancellation_observed_state();
    let server = Server::builder(state)
        .request::<PanicsAfterCancellation, _, _>(panics_after_cancellation)
        .layer(HandlerTimeout(std::time::Duration::from_secs(5)))
        .build()
        .expect("server builds");
    let (incoming_tx, mut outgoing_rx, serve) = start_initialized(server).await;
    incoming_tx
        .send(request(2, PanicsAfterCancellation::METHOD, json!(null)))
        .unwrap();
    started_rx.await.expect("handler started");
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    observed_rx.await.expect("handler observed timeout");

    assert_handler_deadline_expired(&mut outgoing_rx, 2).await;
    stop_and_assert_no_more_responses(&incoming_tx, &mut outgoing_rx, serve, 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_wins_when_handler_returns_success_after_observing_token() {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
    let (started_tx, started_rx) = oneshot::channel();
    let (observed_tx, observed_rx) = oneshot::channel();
    let state = AppState {
        calls: Arc::new(AtomicUsize::new(0)),
        started: Mutex::new(Some(started_tx)),
        cancellation_observed: Mutex::new(Some(observed_tx)),
        release: Arc::new(Notify::new()),
    };
    let server = Server::builder(state)
        .request::<ReturnsAfterCancellation, _, _>(returns_after_cancellation)
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
    let _ = receive_for(&mut outgoing_rx, 1).await;

    incoming_tx
        .send(request(2, ReturnsAfterCancellation::METHOD, json!(null)))
        .unwrap();
    started_rx.await.expect("handler started");
    incoming_tx
        .send(notification("$/cancelRequest", json!({ "id": 2 })))
        .unwrap();
    observed_rx.await.expect("handler observed cancellation");

    assert!(matches!(
        receive_for(&mut outgoing_rx, 2).await,
        RawMessage::Response {
            result: Err(ref error),
            ..
        } if error.code == -32800
    ));

    incoming_tx.send(notification("exit", json!(null))).unwrap();
    serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_releases_capacity_for_a_later_request() {
    let (started_tx, started_rx) = oneshot::channel();
    let (observed_tx, observed_rx) = oneshot::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let state = AppState {
        calls: Arc::clone(&calls),
        started: Mutex::new(Some(started_tx)),
        cancellation_observed: Mutex::new(Some(observed_tx)),
        release: Arc::new(Notify::new()),
    };
    let server = Server::builder(state)
        .request::<ReturnsAfterCancellation, _, _>(returns_after_cancellation)
        .resource_policy(one_slot_policy())
        .build()
        .expect("server builds");
    let (incoming_tx, mut outgoing_rx, serve) = start_initialized(server).await;
    incoming_tx
        .send(request(2, ReturnsAfterCancellation::METHOD, json!(null)))
        .unwrap();
    started_rx.await.expect("first handler started");
    incoming_tx
        .send(notification("$/cancelRequest", json!({ "id": 2 })))
        .unwrap();
    assert!(matches!(
        receive_for(&mut outgoing_rx, 2).await,
        RawMessage::Response {
            result: Err(ref error),
            ..
        } if error.code == -32800
    ));
    observed_rx
        .await
        .expect("first handler observed cancellation");

    let admitted = cancel_first_request_admitted_after_reap(
        &incoming_tx,
        &mut outgoing_rx,
        3..=10,
        ReturnsAfterCancellation::METHOD,
        json!(null),
    )
    .await;
    assert!(
        admitted >= 3,
        "cancellation released capacity after task reap"
    );
    assert!(
        calls.load(Ordering::SeqCst) <= 2,
        "at most the two admitted requests reached user dispatch"
    );

    incoming_tx.send(notification("exit", json!(null))).unwrap();
    serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_cancels_only_admitted_work_after_a_flood() {
    let (started_tx, started_rx) = oneshot::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let state = AppState {
        calls: Arc::clone(&calls),
        started: Mutex::new(Some(started_tx)),
        cancellation_observed: Mutex::new(None),
        release: Arc::new(Notify::new()),
    };
    let server = Server::builder(state)
        .request::<Controlled, _, _>(controlled)
        .resource_policy(one_slot_policy())
        .build()
        .expect("server builds");
    let (incoming_tx, mut outgoing_rx, serve) = start_initialized(server).await;
    incoming_tx
        .send(request(2, Controlled::METHOD, json!("accepted")))
        .unwrap();
    started_rx.await.expect("accepted handler started");
    for id in 3..=34 {
        incoming_tx
            .send(request(id, Controlled::METHOD, json!("excess")))
            .unwrap();
    }
    for id in 3..=34 {
        assert!(matches!(
            receive_for(&mut outgoing_rx, id).await,
            RawMessage::Response {
                result: Err(ref error),
                ..
            } if error.code == -32802
        ));
    }

    drop(incoming_tx);
    serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the flood spawned no extra work"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_versus_success_race_selects_one_response_and_releases_the_id() {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
    let (started_tx, started_rx) = oneshot::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Notify::new());
    let state = AppState {
        calls: Arc::clone(&calls),
        started: Mutex::new(Some(started_tx)),
        cancellation_observed: Mutex::new(None),
        release: Arc::clone(&release),
    };
    let server = Server::builder(state)
        .request::<Controlled, _, _>(controlled)
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
    let _ = receive_for(&mut outgoing_rx, 1).await;

    incoming_tx
        .send(request(2, Controlled::METHOD, json!("race")))
        .unwrap();
    started_rx.await.expect("handler started");
    release.notify_one();
    incoming_tx
        .send(notification("$/cancelRequest", json!({ "id": 2 })))
        .unwrap();

    let first = receive_for(&mut outgoing_rx, 2).await;
    assert!(
        matches!(
            first,
            RawMessage::Response {
                result: Ok(ref result),
                ..
            } if serde_json::from_slice::<String>(result).unwrap() == "race"
        ) || matches!(
            first,
            RawMessage::Response {
                result: Err(ref error),
                ..
            } if error.code == -32800
        ),
        "success or cancellation must win the shared completion gate"
    );

    // Reusing the ID after the race proves the winning completion removed the
    // original registry entry. A late losing path would be observed here as
    // the wrong response body.
    incoming_tx
        .send(request(2, Controlled::METHOD, json!("reused")))
        .unwrap();
    release.notify_one();
    assert!(matches!(
        receive_for(&mut outgoing_rx, 2).await,
        RawMessage::Response {
            result: Ok(ref result),
            ..
        } if serde_json::from_slice::<String>(result).unwrap() == "reused"
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    incoming_tx.send(notification("exit", json!(null))).unwrap();
    serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
    while let Ok(message) = outgoing_rx.try_recv() {
        assert_ne!(
            message.id(),
            Some(&RequestId::Number(2)),
            "a losing race path emitted an additional response"
        );
    }
}

struct InitializeState {
    started: Mutex<Option<oneshot::Sender<()>>>,
    inspect: Arc<Notify>,
    observed: Mutex<Option<oneshot::Sender<bool>>>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_request_is_not_cancellable() {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel();
    let (started_tx, started_rx) = oneshot::channel();
    let (observed_tx, observed_rx) = oneshot::channel();
    let inspect = Arc::new(Notify::new());
    let state = InitializeState {
        started: Mutex::new(Some(started_tx)),
        inspect: Arc::clone(&inspect),
        observed: Mutex::new(Some(observed_tx)),
    };
    let server = Server::builder(state)
        .on_initialize(|state, _ctx, _params, ct| async move {
            if let Some(started) = state.started.lock().await.take() {
                let _ = started.send(());
            }
            state.inspect.notified().await;
            if let Some(observed) = state.observed.lock().await.take() {
                let _ = observed.send(ct.is_cancelled());
            }
            Ok(None)
        })
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
    started_rx.await.expect("initialize hook started");
    incoming_tx
        .send(notification("$/cancelRequest", json!({ "id": 1 })))
        .unwrap();
    inspect.notify_one();

    assert!(
        !observed_rx.await.expect("initialize hook inspected token"),
        "initialize must receive a token that $/cancelRequest cannot trigger"
    );
    assert!(matches!(
        receive_for(&mut outgoing_rx, 1).await,
        RawMessage::Response { result: Ok(_), .. }
    ));

    incoming_tx.send(notification("exit", json!(null))).unwrap();
    serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
    while let Ok(message) = outgoing_rx.try_recv() {
        assert_ne!(
            message.id(),
            Some(&RequestId::Number(1)),
            "cancelling initialize emitted an additional response"
        );
    }
}
