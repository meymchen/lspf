use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lspf::types::notification::Notification;
use lspf::types::request::Request;
use lspf::types::{Uri, WorkDoneProgressCancelParams};
use lspf::{
    CancellationToken, ClientError, LspError, ProgressOptions, RawMessage, ServerContext,
    Transport, TransportError, TransportReader, TransportWriter,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot};

use super::harness::{CountGuard, ResourceCounts};

pub enum Echo {}

impl Request for Echo {
    type Params = EchoParams;
    type Result = String;
    const METHOD: &'static str = "soak/echo";
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EchoParams {
    pub payload: String,
}

#[derive(Clone)]
pub struct RequestState {
    pub counts: ResourceCounts,
    pub release: Arc<Notify>,
}

pub async fn echo(
    state: Arc<RequestState>,
    _context: ServerContext,
    params: EchoParams,
    _cancellation: CancellationToken,
) -> Result<String, LspError> {
    let _task = CountGuard::enter(&state.counts.handler_tasks);
    state.release.notified().await;
    Ok(params.payload)
}

pub enum Stall {}

impl Request for Stall {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "soak/stall";
}

pub async fn stall(
    counts: Arc<ResourceCounts>,
    _context: ServerContext,
    (): (),
    cancellation: CancellationToken,
) -> Result<(), LspError> {
    let _task = CountGuard::enter(&counts.handler_tasks);
    cancellation.cancelled().await;
    std::future::pending().await
}

#[derive(Deserialize, Serialize)]
pub struct DocumentProbeParams {
    pub uri: Uri,
}

pub enum DocumentProbe {}

impl Request for DocumentProbe {
    type Params = DocumentProbeParams;
    type Result = Option<i32>;
    const METHOD: &'static str = "soak/documentVersion";
}

pub async fn document_probe(
    counts: Arc<ResourceCounts>,
    context: ServerContext,
    params: DocumentProbeParams,
    _cancellation: CancellationToken,
) -> Result<Option<i32>, LspError> {
    let _task = CountGuard::enter(&counts.handler_tasks);
    Ok(context
        .documents()
        .get(&params.uri)
        .and_then(|document| document.version()))
}

pub enum ProgressRequest {}

impl Request for ProgressRequest {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "soak/progress";
}

#[derive(Clone)]
pub struct ProgressState {
    pub counts: ResourceCounts,
    pub ended: Arc<Mutex<HashMap<String, CancellationToken>>>,
    pub retained_entry: Arc<AtomicBool>,
    pub hooks_seen: Arc<AtomicUsize>,
    pub hook_notify: Arc<Notify>,
}

pub async fn progress(
    state: Arc<ProgressState>,
    context: ServerContext,
    (): (),
    _cancellation: CancellationToken,
) -> Result<(), LspError> {
    let _task = CountGuard::enter(&state.counts.handler_tasks);
    let handle = context
        .client()
        .begin_progress(ProgressOptions::new("soak").cancellable(true))
        .await
        .map_err(LspError::internal)?;
    let _progress = CountGuard::enter(&state.counts.progress_entries);
    let token = serde_json::to_string(&handle.token()).map_err(LspError::internal)?;
    let cancellation = handle.cancellation_token();
    handle
        .report(Some("running".into()), Some(50))
        .map_err(LspError::internal)?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    handle
        .end(Some("done".into()))
        .map_err(LspError::internal)?;
    state.ended.lock().unwrap().insert(token, cancellation);
    Ok(())
}

pub async fn progress_cancel_hook(
    state: Arc<ProgressState>,
    _context: ServerContext,
    params: WorkDoneProgressCancelParams,
) {
    let key = serde_json::to_string(&params.token).expect("progress token serializes");
    if let Some(cancellation) = state.ended.lock().unwrap().remove(&key)
        && cancellation.is_cancelled()
    {
        state.retained_entry.store(true, Ordering::Release);
    }
    state.hooks_seen.fetch_add(1, Ordering::AcqRel);
    state.hook_notify.notify_waiters();
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct FloodParams {
    pub attempts: usize,
}

pub enum Flood {}

impl Notification for Flood {
    type Params = FloodParams;
    const METHOD: &'static str = "soak/flood";
}

enum SlowMessage {}

impl Notification for SlowMessage {
    type Params = String;
    const METHOD: &'static str = "soak/slowMessage";
}

pub struct SlowPeerState {
    pub counts: Arc<ResourceCounts>,
    pub completed: Mutex<Option<oneshot::Sender<(usize, usize)>>>,
}

pub async fn flood(state: Arc<SlowPeerState>, context: ServerContext, params: FloodParams) {
    let _task = CountGuard::enter(&state.counts.handler_tasks);
    let mut accepted = 0;
    let mut overloaded = 0;
    for _ in 0..params.attempts {
        match context.client().notify::<SlowMessage>("x".repeat(1024)) {
            Ok(()) => accepted += 1,
            Err(ClientError::OutboundOverloaded) => overloaded += 1,
            Err(error) => panic!("unexpected slow-peer failure: {error}"),
        }
    }
    state
        .completed
        .lock()
        .unwrap()
        .take()
        .expect("slow-peer flood runs once")
        .send((accepted, overloaded))
        .ok();
}

pub struct SlowTransport {
    pub incoming: mpsc::UnboundedReceiver<RawMessage>,
    pub outgoing: mpsc::UnboundedSender<RawMessage>,
    pub delay: Duration,
    pub writes_blocked: Arc<AtomicBool>,
    pub write_release: Arc<Semaphore>,
}

pub struct SlowReader(mpsc::UnboundedReceiver<RawMessage>);

pub struct SlowWriter {
    outgoing: mpsc::UnboundedSender<RawMessage>,
    delay: Duration,
    writes_blocked: Arc<AtomicBool>,
    write_release: Arc<Semaphore>,
}

impl Transport for SlowTransport {
    type Reader = SlowReader;
    type Writer = SlowWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            SlowReader(self.incoming),
            SlowWriter {
                outgoing: self.outgoing,
                delay: self.delay,
                writes_blocked: self.writes_blocked,
                write_release: self.write_release,
            },
        )
    }
}

impl TransportReader for SlowReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.0.recv().await.ok_or(TransportError::Closed)
    }
}

impl TransportWriter for SlowWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        if self.writes_blocked.load(Ordering::Acquire) {
            self.write_release
                .acquire()
                .await
                .map_err(|_| TransportError::Closed)?
                .forget();
        }
        tokio::time::sleep(self.delay).await;
        self.outgoing
            .send(message)
            .map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}
