//! Connection-owned protocol engine for `Server<S>`.
//!
//! This slice serves a connection end to end for the lifecycle plus typed
//! custom requests, notifications, and commands. `initialize` is the one
//! bounded transaction that can conditionally extend the Router, freeze it,
//! generate capabilities, establish the connection's [`Workspace`],
//! [`Documents`], and negotiated position encoding, and run the
//! `on_initialize` lifecycle hook — all without exposing partial state
//! (ADR 0017, ADR 0018). The later lifecycle notifications carry the remaining
//! hooks: the client's `initialized` runs `on_initialized` once, in the
//! running state only, a successful `on_shutdown` gates the transition into
//! shutting down, and the peer's `exit` runs `on_exit` before the engine
//! computes the exit outcome (ADR 0024) — the exit hook resolves to `()`, so it
//! cannot change the exit code the lifecycle implies. Inbound
//! requests reserve their IDs before user work is spawned; the engine's atomic
//! completion gate then arbitrates success, errors, and cancellation.
//!
//! Every way a connection can end — reader EOF, a reader error, a writer send
//! or shutdown failure, `exit`, and the fatal termination a failed initialize
//! transaction takes — requests the same idempotent close operation. The first
//! requester records the [`CloseCause`] and wakes the read-loop; the engine
//! then performs the cleanup exactly once and reports the recorded cause as an
//! [`Outcome`] or a transport [`Error`]. The engine never terminates the
//! process; the entry point decides what an [`Outcome`] means for a binary.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures_channel::mpsc::UnboundedReceiver;
use futures_util::FutureExt;
use futures_util::future::{Either, select};
use futures_util::select_biased;
use lsp_types::error_codes::SERVER_CANCELLED;
use lsp_types::{
    DidChangeConfigurationParams, DidChangeTextDocumentParams, DidChangeWorkspaceFoldersParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    InitializeParams, InitializedParams, OneOf, ServerInfo, SetTraceParams, TextDocumentSyncKind,
    TextDocumentSyncOptions, TextDocumentSyncSaveOptions, WillSaveTextDocumentParams,
    WorkDoneProgressCancelParams, WorkspaceFoldersServerCapabilities, WorkspaceServerCapabilities,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, debug, warn};

use crate::builder::{
    ConfigureInitialize, InitializeRegistrar, OnExit, OnInitialize, OnInitialized, OnShutdown,
    ProtocolNotification, Registrations, Server,
};
use crate::capability::GeneratedCapabilities;
use crate::client::{Client, OutboundQueue, OutboundRegistry};
use crate::codec::{decode_params, decode_value, encode_body};
use crate::context::Context;
use crate::documents::Documents;
use crate::error::Error;
use crate::failure::{ConnectionDirection, ConnectionFailureCategory, FailureReporter};
use crate::file_provider::SharedFileProvider;
use crate::progress::{ProgressCancel, ProgressRegistry};
use crate::raw::{JsonRpcError, RawMessage, RequestId};
use crate::runtime::{Runtime, TaskHandle, TaskSend, default_runtime, ensure_runtime_available};
use crate::service::{
    HandlerTimeout, IncomingCall, ServiceResult, UserLayer, UserService, build_service_stack,
};
use crate::sync::{OwnedPermit, Semaphore};
use crate::telemetry::{
    Completion, ConnectionTrace, DeadlineAction, Direction, Instant, Resource, ResourceAction,
};
use crate::transport::{Transport, TransportError, TransportReader, TransportWriter};
use crate::workspace::Workspace;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireInitializeResult {
    capabilities: GeneratedCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    server_info: Option<ServerInfo>,
}

use crate::{LspError, Result};

fn validate_sync_changes(
    kind: TextDocumentSyncKind,
    changes: &[lsp_types::TextDocumentContentChangeEvent],
) -> std::result::Result<(), LspError> {
    if kind == TextDocumentSyncKind::INCREMENTAL {
        return Ok(());
    }
    if kind == TextDocumentSyncKind::FULL {
        return if changes.iter().all(|change| change.range.is_none()) {
            Ok(())
        } else {
            Err(LspError::invalid_request(
                "range changes require incremental document synchronization",
            ))
        };
    }
    Err(LspError::invalid_request(
        "document changes are disabled by the configured synchronization kind",
    ))
}

/// Decode the client's `initialized` notification params.
///
/// LSP 3.17 defines `InitializedParams` as an empty object, and clients send
/// either `{}` or no params at all (JSON-RPC `null`). The `lsp-types` 0.97
/// type is a unit struct, so its derived deserializer accepts only `null`;
/// accepting the empty object too keeps the typed hook reachable from every
/// real client.
fn decode_initialized_params(raw: &Bytes) -> std::result::Result<InitializedParams, LspError> {
    match serde_json::from_slice::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Null) => Ok(InitializedParams {}),
        Ok(serde_json::Value::Object(map)) if map.is_empty() => Ok(InitializedParams {}),
        _ => Err(LspError::invalid_params(
            "initialized params must be an empty object",
        )),
    }
}

/// How one connection ended.
///
/// Serving a connection resolves to exactly one `Outcome` or to a transport
/// [`Error`]; it never terminates the process. A server binary maps the
/// outcome to a process disposition itself — [`Outcome::code`] reports the
/// exit code the LSP lifecycle implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The peer sent `exit`. `code` is the LSP exit code: 0 when `shutdown`
    /// completed first, 1 otherwise.
    Exit {
        /// The process exit code prescribed by the LSP lifecycle.
        code: i32,
    },
    /// The peer closed the transport before sending `exit`.
    TransportClosed,
    /// The outbound path failed terminally: the writer failed, or required
    /// protocol traffic could not fit within the configured queue budgets.
    WriterFailed,
    /// A failed initialize transaction terminated the connection after its
    /// fixed error response was enqueued (ADR 0018).
    InitializeFailed,
}

impl Outcome {
    /// The process exit code this outcome implies for a server binary: the
    /// LSP-defined code after `exit`, and 1 for every ending without one.
    pub fn code(&self) -> i32 {
        match self {
            Self::Exit { code } => *code,
            Self::TransportClosed | Self::WriterFailed | Self::InitializeFailed => 1,
        }
    }
}

/// What first requested the engine's one close operation.
///
/// Only the first requester's cause is recorded, so a writer failure racing
/// reader EOF still reports a single deterministic ending.
#[derive(Debug)]
enum CloseCause {
    /// An `exit` notification was processed; carries the LSP exit code.
    Exit { code: i32 },
    /// The reader reached end of input before `exit`.
    ReaderEof,
    /// The reader failed with a transport error.
    ReaderFailed(TransportError),
    /// The writer failed to send or shut down, or required protocol traffic
    /// could not fit within the outbound resource policy.
    WriterFailed,
    /// A failed initialize transaction terminated the connection (ADR 0018).
    InitializeFailed,
}

impl CloseCause {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Exit { .. } => "exit",
            Self::ReaderEof => "reader_eof",
            Self::ReaderFailed(_) => "reader_failed",
            Self::WriterFailed => "writer_failed",
            Self::InitializeFailed => "initialize_failed",
        }
    }

    /// Map the recorded cause onto what serving the connection returns.
    fn into_result(self) -> Result<Outcome> {
        match self {
            Self::Exit { code } => Ok(Outcome::Exit { code }),
            Self::ReaderEof => Ok(Outcome::TransportClosed),
            Self::ReaderFailed(error) => Err(Error::Transport(error)),
            Self::WriterFailed => Ok(Outcome::WriterFailed),
            Self::InitializeFailed => Ok(Outcome::InitializeFailed),
        }
    }
}

/// The engine-owned request to close the session, shared with the writer task.
///
/// It performs no cleanup of its own: the writer and the read-loop only
/// *request* closure through it, and [`ProtocolEngine::close`] remains the sole
/// place that clears registries, cancels tasks, and closes the queue
/// (ADR 0018).
#[derive(Clone)]
struct CloseSignal {
    inner: Arc<CloseInner>,
}

struct CloseInner {
    cause: Mutex<Option<CloseCause>>,
    requested: CancellationToken,
}

impl CloseSignal {
    fn new() -> Self {
        Self {
            inner: Arc::new(CloseInner {
                cause: Mutex::new(None),
                requested: CancellationToken::new(),
            }),
        }
    }

    /// Request the one close operation. The first caller records the
    /// provisional `cause` and wakes the read-loop; a later caller leaves it
    /// untouched and observes that same close rather than starting a second
    /// one. Required outbound admission failure can override this provisional
    /// cause when the final outcome is selected (ADR 0026).
    fn request(&self, cause: CloseCause) {
        {
            let mut recorded = self.inner.cause.lock().unwrap();
            if recorded.is_none() {
                *recorded = Some(cause);
            }
        }
        self.inner.requested.cancel();
    }

    /// The token that fires once any caller has requested closure.
    fn requested(&self) -> CancellationToken {
        self.inner.requested.clone()
    }

    /// Take the recorded cause. Called once, by the read-loop, after the close
    /// operation has run.
    fn take_cause(&self) -> Option<CloseCause> {
        self.inner.cause.lock().unwrap().take()
    }
}

/// Drive a [`Server`] over `transport` until the peer exits, the transport
/// closes, a transport error ends the session, or a failed initialize
/// transaction enters the terminal close path.
///
/// The writer half moves into a send-loop task draining an unbounded channel;
/// the read-loop owns the reader and processes one envelope at a time.
pub(crate) async fn run<S, T>(server: Server<S>, transport: T) -> Result<Outcome>
where
    S: Send + Sync + 'static,
    T: Transport,
{
    // Serving must not begin before the target's executor exists: a missing
    // Tokio runtime is reported as an error instead of panicking inside the
    // first spawn (ADR 0020). The framework never starts a runtime implicitly.
    ensure_runtime_available()?;
    let connection_trace = ConnectionTrace::new();
    let failure_reporter = FailureReporter::new(server.error_hook.clone(), connection_trace.id());
    let connection_span = connection_trace.span();
    let (reader, writer) = transport.split();
    let (out_tx, out_rx) = OutboundQueue::bounded_with_reporter(
        server.resource_policy.max_outbound_messages,
        server.resource_policy.max_outbound_bytes,
        connection_trace,
        failure_reporter.clone(),
    );
    let client = Client::new(
        out_tx.clone(),
        OutboundRegistry::default(),
        server.resource_policy.outbound_request_timeout,
    );
    let close = CloseSignal::new();
    let runtime = default_runtime();
    let send_task = runtime.spawn(
        send_loop_with_trace(
            writer,
            out_rx,
            client.clone(),
            close.clone(),
            connection_trace,
            failure_reporter.clone(),
        )
        .instrument(connection_span.clone()),
    );
    ProtocolEngine::new(
        server,
        runtime,
        out_tx,
        client,
        close,
        send_task,
        connection_trace,
        failure_reporter,
    )
    .serve(reader)
    .instrument(connection_span)
    .await
}

struct TaskGroup<R> {
    runtime: R,
    handles: Vec<InboundTask>,
}

struct InboundTask {
    handle: TaskHandle,
    _permit: Arc<OwnedPermit>,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
struct RequestGeneration(u64);

impl RequestGeneration {
    fn take_next(&mut self) -> Self {
        let generation = *self;
        self.0 += 1;
        generation
    }
}

impl<R: Runtime> TaskGroup<R> {
    fn new(runtime: R) -> Self {
        Self {
            runtime,
            handles: Vec::new(),
        }
    }

    fn spawn<F>(&mut self, future: F, permit: Arc<OwnedPermit>)
    where
        F: Future<Output = ()> + TaskSend + 'static,
    {
        self.handles.push(InboundTask {
            handle: self.runtime.spawn(future),
            _permit: permit,
        });
    }

    async fn reap_finished(&mut self) {
        let mut running = Vec::with_capacity(self.handles.len());
        for task in std::mem::take(&mut self.handles) {
            if task.handle.is_finished() {
                task.handle.join().await;
            } else {
                running.push(task);
            }
        }
        self.handles = running;
    }

    async fn abort_and_join(&mut self) {
        for task in &self.handles {
            task.handle.abort();
        }
        self.join_all().await;
    }

    async fn join_all(&mut self) {
        for task in std::mem::take(&mut self.handles) {
            task.handle.join().await;
        }
    }
}

/// One accepted inbound request: its wire ID plus the generation that claimed
/// that ID.
///
/// A peer may legitimately reuse a request ID once the previous request with
/// that ID has been answered, so the ID alone does not identify a request for
/// the lifetime of its task. The generation makes the completion gate
/// identity-scoped: a task whose result arrives after its own entry was claimed
/// — by `$/cancelRequest`, by `shutdown`, or by session close — cannot then
/// claim the entry a later request has since reserved under the same ID.
struct Reservation {
    id: RequestId,
    method: String,
    started: Instant,
    generation: RequestGeneration,
    _permit: Arc<OwnedPermit>,
}

struct InboundEntry {
    method: String,
    started: Instant,
    generation: RequestGeneration,
    /// `None` for `initialize`, the one request that is not cancellable.
    cancellation: Option<CancellationToken>,
    _permit: Arc<OwnedPermit>,
}

struct InboundInner {
    entries: HashMap<RequestId, InboundEntry>,
    next_generation: RequestGeneration,
}

#[derive(Clone)]
struct InboundRegistry {
    inner: Arc<Mutex<InboundInner>>,
    capacity: Arc<Semaphore>,
    trace: ConnectionTrace,
    failure_reporter: FailureReporter,
    limit: usize,
}

#[derive(Debug)]
enum InboundReserveError {
    DuplicateId,
    CapacityExhausted,
}

const INBOUND_CAPACITY_EXHAUSTED: &str = "inbound request capacity exhausted";
const HANDLER_DEADLINE_EXPIRED: &str = "handler deadline expired";

struct ReservedRequest {
    reservation: Reservation,
    cancellation: Option<CancellationToken>,
}

impl InboundRegistry {
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn new(capacity: usize) -> Self {
        let trace = ConnectionTrace::new();
        Self::new_with_reporter(capacity, trace, FailureReporter::new(None, trace.id()))
    }

    fn new_with_reporter(
        capacity: usize,
        trace: ConnectionTrace,
        failure_reporter: FailureReporter,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(InboundInner {
                entries: HashMap::new(),
                next_generation: RequestGeneration::default(),
            })),
            capacity: Semaphore::shared(capacity),
            trace,
            failure_reporter,
            limit: capacity,
        }
    }

    /// Reserve capacity and `id` before allocating request-scoped cancellation
    /// state. A duplicate never replaces or cancels the original (ADR 0018),
    /// and an exhausted connection never grows its registry or task group.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn reserve(
        &self,
        id: RequestId,
        cancellation_parent: Option<&CancellationToken>,
    ) -> std::result::Result<ReservedRequest, InboundReserveError> {
        self.reserve_method(id, "test/request", cancellation_parent)
    }

    fn reserve_method(
        &self,
        id: RequestId,
        method: &str,
        cancellation_parent: Option<&CancellationToken>,
    ) -> std::result::Result<ReservedRequest, InboundReserveError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.entries.contains_key(&id) {
            self.trace.resource_budget(
                Resource::InboundRequests,
                ResourceAction::Reject,
                inner.entries.len(),
                self.limit,
                None,
            );
            return Err(InboundReserveError::DuplicateId);
        }
        let Some(permit) = self.capacity.try_acquire_owned() else {
            let current = inner.entries.len();
            self.trace.resource_budget(
                Resource::InboundRequests,
                ResourceAction::Reject,
                current,
                self.limit,
                None,
            );
            drop(inner);
            self.failure_reporter.report(
                ConnectionFailureCategory::Overload,
                Some(ConnectionDirection::Inbound),
                Some(method),
                Some(&id),
            );
            return Err(InboundReserveError::CapacityExhausted);
        };
        let permit = Arc::new(permit);
        let cancellation = cancellation_parent.map(CancellationToken::child_token);
        let started = Instant::now();
        let generation = inner.next_generation.take_next();
        inner.entries.insert(
            id.clone(),
            InboundEntry {
                method: method.to_string(),
                started,
                generation,
                cancellation: cancellation.clone(),
                _permit: Arc::clone(&permit),
            },
        );
        let current = inner.entries.len();
        drop(inner);
        self.trace.resource_budget(
            Resource::InboundRequests,
            ResourceAction::Admit,
            current,
            self.limit,
            None,
        );
        Ok(ReservedRequest {
            reservation: Reservation {
                id,
                method: method.to_string(),
                started,
                generation,
                _permit: permit,
            },
            cancellation,
        })
    }

    /// Claim the completion gate for `reservation` and enqueue its one response.
    /// Does nothing if some other path already claimed that entry.
    fn complete(
        &self,
        out_tx: &OutboundQueue,
        reservation: Reservation,
        result: std::result::Result<Bytes, LspError>,
    ) {
        let current = {
            let mut inner = self.inner.lock().unwrap();
            match inner.entries.get(&reservation.id) {
                Some(entry) if entry.generation == reservation.generation => {
                    inner.entries.remove(&reservation.id);
                    Some(inner.entries.len())
                }
                _ => None,
            }
        };
        if let Some(current) = current {
            self.trace.resource_budget(
                Resource::InboundRequests,
                ResourceAction::Release,
                current,
                self.limit,
                None,
            );
            let completion = completion_kind(&result);
            self.trace.request_completed(
                &reservation.method,
                &reservation.id,
                reservation.started,
                Direction::Inbound,
                completion,
            );
            enqueue_encoded(out_tx, reservation.id, result);
        }
    }

    fn claim_cancellation(&self, id: &RequestId) -> Option<Reservation> {
        let claimed = {
            let mut inner = self.inner.lock().unwrap();
            let entry = match inner.entries.get(id) {
                Some(entry) if entry.cancellation.is_some() => inner.entries.remove(id),
                _ => None,
            };
            entry.map(|entry| (entry, inner.entries.len()))
        };
        claimed.map(|(entry, current)| {
            self.trace.resource_budget(
                Resource::InboundRequests,
                ResourceAction::Release,
                current,
                self.limit,
                None,
            );
            self.trace.request_completed(
                &entry.method,
                id,
                entry.started,
                Direction::Inbound,
                Completion::Cancelled,
            );
            let token = entry
                .cancellation
                .expect("only cancellable entries are claimed");
            token.cancel();
            Reservation {
                id: id.clone(),
                method: entry.method,
                started: entry.started,
                generation: entry.generation,
                _permit: entry._permit,
            }
        })
    }

    /// Cancel and answer every still-registered request, emptying the registry.
    ///
    /// Used by a successful `shutdown`, which leaves the connection alive long
    /// enough to deliver each cancellation. Removing the entry also claims the
    /// completion gate, so the handler's own late result is dropped and every
    /// cancelled request still receives exactly one response.
    fn cancel_all_with_response(&self, out_tx: &OutboundQueue) {
        let entries = std::mem::take(&mut self.inner.lock().unwrap().entries);
        if !entries.is_empty() {
            self.trace.resource_budget(
                Resource::InboundRequests,
                ResourceAction::Release,
                0,
                self.limit,
                None,
            );
        }
        for (id, entry) in entries {
            self.trace.request_completed(
                &entry.method,
                &id,
                entry.started,
                Direction::Inbound,
                Completion::Cancelled,
            );
            if let Some(cancellation) = entry.cancellation {
                cancellation.cancel();
            }
            enqueue_encoded(out_tx, id, Err(LspError::RequestCancelled));
        }
    }

    /// Cancel every still-registered request and empty the registry without
    /// answering.
    ///
    /// Used by session close, where the peer has either gone away or asked to
    /// exit: there is no one left to receive a cancellation. `shutdown` is the
    /// one ending that still answers, through
    /// [`cancel_all_with_response`](Self::cancel_all_with_response).
    fn close_all(&self) {
        let entries = std::mem::take(&mut self.inner.lock().unwrap().entries);
        if !entries.is_empty() {
            self.trace.resource_budget(
                Resource::InboundRequests,
                ResourceAction::Release,
                0,
                self.limit,
                None,
            );
        }
        for (id, entry) in entries {
            self.trace.request_completed(
                &entry.method,
                &id,
                entry.started,
                Direction::Inbound,
                Completion::ConnectionClosed,
            );
            if let Some(cancellation) = entry.cancellation {
                cancellation.cancel();
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct CancelParams {
    id: RequestId,
}

/// Whether a protocol built-in's post-validation hook runs.
///
/// Most built-ins gate their hook on the `Result` of
/// [`ProtocolEngine::process_protocol_notification`]; the work-done progress
/// cancel built-in reports its own non-error rejections (malformed params) at
/// debug level and signals the gate directly instead.
enum BuiltInGate {
    /// Decode — and any mutation — succeeded: dispatch the registered hook.
    RunHook,
    /// The notification was dropped before decode completed: no hook runs.
    SkipHook,
}

/// Decode and apply one `window/workDoneProgress/cancel` notification against
/// the connection's progress registry (ADR 0018).
///
/// A matching active and cancellable token fires the handle's cancellation
/// token; cancellation never sends a work-done end by itself — the
/// application decides the final message and calls `end`. Unknown, ended, and
/// non-cancellable tokens, like malformed params, are logged at debug level
/// and otherwise ignored, leaving the connection usable. The hook gate opens
/// only after a successful decode, so a registered hook always observes the
/// updated cancellation state.
fn gate_progress_cancel(registry: &ProgressRegistry, raw_params: &Bytes) -> BuiltInGate {
    let params = match decode_params::<WorkDoneProgressCancelParams>(raw_params) {
        Ok(params) => params,
        Err(error) => {
            debug!(%error, "ignoring malformed window/workDoneProgress/cancel");
            return BuiltInGate::SkipHook;
        }
    };
    match registry.cancel(&params.token) {
        ProgressCancel::Cancelled => {}
        ProgressCancel::NotCancellable => debug!(
            token = ?params.token,
            "ignoring work-done progress cancel for a non-cancellable token"
        ),
        ProgressCancel::NotActive => debug!(
            token = ?params.token,
            "ignoring work-done progress cancel for an unknown or ended token"
        ),
    }
    BuiltInGate::RunHook
}

#[cfg(all(test, not(target_arch = "wasm32")))]
async fn send_loop<W: TransportWriter>(
    writer: W,
    out_rx: UnboundedReceiver<RawMessage>,
    client: Client,
    close: CloseSignal,
) {
    let trace = ConnectionTrace::new();
    send_loop_with_trace(
        writer,
        out_rx,
        client,
        close,
        trace,
        FailureReporter::new(None, trace.id()),
    )
    .await;
}

async fn send_loop_with_trace<W: TransportWriter>(
    mut writer: W,
    mut out_rx: UnboundedReceiver<RawMessage>,
    client: Client,
    close: CloseSignal,
    trace: ConnectionTrace,
    failure_reporter: FailureReporter,
) {
    let outbound_closing = client.outbound_closing();
    loop {
        let msg = select_biased! {
            msg = out_rx.recv().fuse() => msg,
            () = outbound_closing.cancelled().fuse() => {
                out_rx.close();
                break;
            }
        };
        // A closed channel (its receiver half dropped) is a `RecvError`: the
        // engine has shut the queue down, so nothing further can be enqueued.
        let Ok(msg) = msg else {
            client.close_outbound();
            break;
        };
        // The depth counts what is still queued, so each message is decremented
        // once its transport send has succeeded or failed — including the
        // terminally failed send, after which the loop returns.
        trace.message(Direction::Outbound, &msg);
        let method = msg.method().map(str::to_owned);
        let request_id = msg.id().cloned();
        let sent = writer.send(msg).await;
        client.record_done();
        if let Err(e) = sent {
            failure_reporter.report(
                ConnectionFailureCategory::Transport,
                Some(ConnectionDirection::Outbound),
                method.as_deref(),
                request_id.as_ref(),
            );
            warn!(error = %e, "send_loop: transport write failed");
            out_rx.close();
            client.discard_outbound();
            // ADR 0018: the writer reports its terminal failure and performs no
            // registry or task cleanup of its own; the engine runs the one
            // close operation. Accounting is released here because the
            // receiver is abandoning every message it retained.
            close.request(CloseCause::WriterFailed);
            return;
        }
    }
    while let Ok(msg) = out_rx.recv().await {
        trace.message(Direction::Outbound, &msg);
        let method = msg.method().map(str::to_owned);
        let request_id = msg.id().cloned();
        let sent = writer.send(msg).await;
        client.record_done();
        if let Err(e) = sent {
            failure_reporter.report(
                ConnectionFailureCategory::Transport,
                Some(ConnectionDirection::Outbound),
                method.as_deref(),
                request_id.as_ref(),
            );
            warn!(error = %e, "send_loop: transport write failed while draining");
            out_rx.close();
            client.discard_outbound();
            close.request(CloseCause::WriterFailed);
            return;
        }
    }
    if let Err(e) = writer.shutdown().await {
        failure_reporter.report(
            ConnectionFailureCategory::Close,
            Some(ConnectionDirection::Outbound),
            None,
            None,
        );
        warn!(error = %e, "send_loop: transport shutdown failed");
        close.request(CloseCause::WriterFailed);
    }
}

/// The static registrations and lifecycle callbacks awaiting the initialize
/// transaction. Held only while the connection is [`Lifecycle::Uninitialized`];
/// the transaction consumes it once, so it need not be `Clone`.
struct Pending<S> {
    registrations: Registrations<S>,
    file_provider: SharedFileProvider,
    configure_initialize: Option<ConfigureInitialize<S>>,
    on_initialize: Option<OnInitialize<S>>,
    on_initialized: Option<OnInitialized<S>>,
    on_shutdown: Option<OnShutdown<S>>,
    on_exit: Option<OnExit<S>>,
    layers: Vec<UserLayer<S>>,
    concurrency_limit: usize,
}

/// The connection's lifecycle phase. The frozen [`Router`] exists only after a
/// successful initialize transaction, so it lives inside [`Lifecycle::Running`]
/// rather than being available up front.
enum Lifecycle<S> {
    Uninitialized(Box<Pending<S>>),
    Initializing,
    Running(UserService<S>),
    ShuttingDown,
    Exited,
}

/// The single owner of mutable protocol coordination for one connection.
///
/// Transport code only feeds envelopes in and drains envelopes out. Lifecycle
/// selection, request registration, cancellation, task ownership, terminal
/// response arbitration, and session close all remain behind this boundary.
struct ProtocolEngine<S, R> {
    state: Arc<S>,
    documents: Documents,
    workspace: Option<Workspace>,
    document_sync: TextDocumentSyncOptions,
    lifecycle: Lifecycle<S>,
    /// The `on_initialized` hook awaiting the client's `initialized`
    /// notification. Lifted out of [`Pending`] when the initialize transaction
    /// succeeds, and taken by the first running-state `initialized` message.
    on_initialized: Option<OnInitialized<S>>,
    /// The `on_shutdown` hook for the running state. Unlike notification hooks,
    /// it is reusable after an error because a failed attempt leaves the
    /// connection running and the client may send shutdown again.
    on_shutdown: Option<OnShutdown<S>>,
    /// The `on_exit` hook awaiting the peer's `exit` notification. Lifted out
    /// of [`Pending`] when the initialize transaction succeeds; an `exit`
    /// received earlier closes without a Workspace to hand it.
    on_exit: Option<OnExit<S>>,
    inbound: InboundRegistry,
    handler_timeout: Duration,
    tasks: TaskGroup<R>,
    out_tx: OutboundQueue,
    client: Client,
    /// Cancelled once by [`close`](Self::close). Every request-scoped token is
    /// a child of it, so closing the session cancels all outstanding user work
    /// even where the completion gate has already claimed its registry entry.
    session: CancellationToken,
    close: CloseSignal,
    trace: ConnectionTrace,
    failure_reporter: FailureReporter,
    /// The writer's send-loop task. Signalled by closing the outbound queue and
    /// then joined by [`close`](Self::close), so it is never detached.
    send_task: Option<TaskHandle>,
}

impl<S, R> ProtocolEngine<S, R>
where
    S: Send + Sync + 'static,
    R: Runtime,
{
    fn new(
        server: Server<S>,
        runtime: R,
        out_tx: OutboundQueue,
        client: Client,
        close: CloseSignal,
        send_task: TaskHandle,
        trace: ConnectionTrace,
        failure_reporter: FailureReporter,
    ) -> Self {
        let max_inbound_requests = server.resource_policy.max_inbound_requests;
        let handler_timeout = server.resource_policy.handler_timeout;
        Self {
            state: server.state,
            documents: Documents::with_resource_policy(server.resource_policy, trace),
            workspace: None,
            // Document notifications are processed only after initialize has
            // replaced this with the validated effective configuration.
            document_sync: TextDocumentSyncOptions::default(),
            lifecycle: Lifecycle::Uninitialized(Box::new(Pending {
                registrations: server.registrations,
                file_provider: server.file_provider,
                configure_initialize: server.configure_initialize,
                on_initialize: server.on_initialize,
                on_initialized: server.on_initialized,
                on_shutdown: server.on_shutdown,
                on_exit: server.on_exit,
                layers: server.layers,
                concurrency_limit: max_inbound_requests,
            })),
            on_initialized: None,
            on_shutdown: None,
            on_exit: None,
            inbound: InboundRegistry::new_with_reporter(
                max_inbound_requests,
                trace,
                failure_reporter.clone(),
            ),
            handler_timeout,
            tasks: TaskGroup::new(runtime),
            out_tx,
            client,
            session: CancellationToken::new(),
            close,
            trace,
            failure_reporter,
            send_task: Some(send_task),
        }
    }

    /// Own the reader and process one envelope at a time until some cause
    /// requests closure, then run the one close operation and report the
    /// ending.
    ///
    /// The read-loop also waits on the close signal, so a writer failure ends
    /// the session without waiting for the peer to send another message.
    async fn serve<Rd>(mut self, mut reader: Rd) -> Result<Outcome>
    where
        Rd: TransportReader,
    {
        let requested = self.close.requested();
        let outbound_failed = self.out_tx.failure();
        loop {
            let msg = select_biased! {
                // `biased`: an already-requested close wins over a message that
                // happens to be ready, so the ending stays deterministic.
                () = requested.cancelled().fuse() => break,
                () = outbound_failed.cancelled().fuse() => {
                    self.close.request(CloseCause::WriterFailed);
                    break;
                },
                msg = reader.recv().fuse() => msg,
            };
            // Reap immediately before dispatch so a completed handler releases
            // its task-owned admission permit before the next request tries to
            // reserve capacity. Finished handles cannot accumulate behind an
            // idle read loop.
            self.tasks.reap_finished().await;

            match msg {
                Ok(msg) => {
                    self.trace.message(Direction::Inbound, &msg);
                    match self.dispatch(msg).await {
                        Flow::Continue => {}
                        Flow::Close(cause) => {
                            self.close.request(cause);
                            break;
                        }
                    }
                }
                Err(TransportError::Closed) => {
                    warn!("transport closed by peer before exit notification");
                    self.close.request(CloseCause::ReaderEof);
                    break;
                }
                Err(error) => {
                    let category = match &error {
                        TransportError::Malformed(_) | TransportError::OversizedMessage { .. } => {
                            ConnectionFailureCategory::Framing
                        }
                        TransportError::Io(_) | TransportError::Serde(_) => {
                            ConnectionFailureCategory::Transport
                        }
                        TransportError::Closed => unreachable!("closed is handled above"),
                    };
                    self.failure_reporter.report(
                        category,
                        Some(ConnectionDirection::Inbound),
                        None,
                        None,
                    );
                    self.close.request(CloseCause::ReaderFailed(error));
                    break;
                }
            }
        }

        self.close().await;
        let cause = final_close_cause(&self.close, &self.out_tx);
        self.trace.connection_closed(cause.as_str());
        cause.into_result()
    }

    async fn dispatch(&mut self, msg: RawMessage) -> Flow {
        match msg {
            RawMessage::Request { id, method, params } => {
                let span = self.trace.request_span(method.as_ref(), &id);
                // Admission happens before request-scoped cancellation state,
                // parameter decoding, and runtime task creation. The owned
                // permit remains attached to the task handle until the engine
                // reaps it, even if cancellation or close claims the response
                // gate first.
                let cancellation_parent = (method != "initialize").then_some(&self.session);
                let reserved = match self.inbound.reserve_method(
                    id.clone(),
                    method.as_ref(),
                    cancellation_parent,
                ) {
                    Ok(reserved) => reserved,
                    Err(InboundReserveError::DuplicateId) => {
                        self.trace.request_completed(
                            method.as_ref(),
                            &id,
                            Instant::now(),
                            Direction::Inbound,
                            Completion::Rejected,
                        );
                        enqueue_error(
                            &self.out_tx,
                            id,
                            LspError::invalid_request("duplicate request id"),
                        );
                        return Flow::Continue;
                    }
                    Err(InboundReserveError::CapacityExhausted) => {
                        self.trace.request_completed(
                            method.as_ref(),
                            &id,
                            Instant::now(),
                            Direction::Inbound,
                            Completion::Rejected,
                        );
                        enqueue_error(
                            &self.out_tx,
                            id,
                            LspError::ServerError {
                                code: SERVER_CANCELLED as i32,
                                message: INBOUND_CAPACITY_EXHAUSTED.to_string(),
                                data: None,
                            },
                        );
                        return Flow::Continue;
                    }
                };
                let reservation = reserved.reservation;
                let cancellation = reserved.cancellation;

                // Initialize precedence: until `initialize` completes, refuse
                // every other request with `ServerNotInitialized`.
                if method != "initialize"
                    && matches!(
                        self.lifecycle,
                        Lifecycle::Uninitialized(_) | Lifecycle::Initializing
                    )
                {
                    self.inbound.complete(
                        &self.out_tx,
                        reservation,
                        Err(LspError::ServerNotInitialized),
                    );
                    return Flow::Continue;
                }
                // After `shutdown`, every request is invalid until `exit`.
                if matches!(self.lifecycle, Lifecycle::ShuttingDown | Lifecycle::Exited) {
                    self.inbound.complete(
                        &self.out_tx,
                        reservation,
                        Err(LspError::invalid_request("invalid request")),
                    );
                    return Flow::Continue;
                }

                match method.as_ref() {
                    "initialize" => return self.initialize(&span, reservation, params).await,
                    "shutdown" => {
                        let params_result = if params.is_empty() {
                            Ok(())
                        } else {
                            decode_params::<()>(&params)
                        };
                        if let Err(err) = params_result {
                            self.failure_reporter.report(
                                ConnectionFailureCategory::Protocol,
                                Some(ConnectionDirection::Inbound),
                                Some("shutdown"),
                                Some(&reservation.id),
                            );
                            self.inbound.complete(&self.out_tx, reservation, Err(err));
                            return Flow::Continue;
                        }
                        if let Some(hook) = &self.on_shutdown {
                            let cancellation = cancellation
                                .expect("shutdown is a cancellable non-initialize request");
                            let ctx = Context::for_request(
                                reservation.id.clone(),
                                span.clone(),
                                self.client.clone(),
                                self.established_workspace(),
                            )
                            .with_cancellation(cancellation.clone());
                            if let Err(err) = hook
                                .invoke((Arc::clone(&self.state), ctx, (), cancellation))
                                .instrument(span.clone())
                                .await
                            {
                                self.inbound.complete(&self.out_tx, reservation, Err(err));
                                return Flow::Continue;
                            }
                        }
                        // The successful shutdown request answers itself first,
                        // so its own entry is gone before the sweep below; only
                        // then cancel the rest of the in-flight work and enter
                        // `ShuttingDown`.
                        self.inbound.complete(
                            &self.out_tx,
                            reservation,
                            encode_body(&serde_json::Value::Null),
                        );
                        self.inbound.cancel_all_with_response(&self.out_tx);
                        self.lifecycle = Lifecycle::ShuttingDown;
                    }
                    _other => {
                        // Precedence guarantees the connection is running here.
                        let service = match &self.lifecycle {
                            Lifecycle::Running(service) => Arc::clone(service),
                            _ => {
                                self.inbound.complete(
                                    &self.out_tx,
                                    reservation,
                                    Err(LspError::ServerNotInitialized),
                                );
                                return Flow::Continue;
                            }
                        };
                        let params = match decode_value(&params) {
                            Ok(params) => params,
                            Err(error) => {
                                self.failure_reporter.report(
                                    ConnectionFailureCategory::Protocol,
                                    Some(ConnectionDirection::Inbound),
                                    Some(method.as_ref()),
                                    Some(&reservation.id),
                                );
                                self.inbound.complete(&self.out_tx, reservation, Err(error));
                                return Flow::Continue;
                            }
                        };
                        self.spawn_service_request(
                            service,
                            span,
                            reservation,
                            method.into_owned(),
                            params,
                            cancellation.expect("non-initialize requests are cancellable"),
                            self.handler_timeout,
                        );
                    }
                }
            }
            RawMessage::Notification { method, params } => match method.as_ref() {
                "exit" => {
                    // The exit hook observes the ending first (ADR 0018,
                    // ADR 0024): it runs after a successful initialize
                    // transaction, before the engine computes the exit
                    // outcome. It resolves to `()`, so it cannot change that
                    // outcome — the LSP exit code below derives from
                    // protocol-owned lifecycle state alone, and the hook
                    // receives only the shared state and a live `Context`.
                    if matches!(
                        self.lifecycle,
                        Lifecycle::Running(_) | Lifecycle::ShuttingDown
                    ) && let Some(hook) = self.on_exit.take()
                    {
                        let span = self.trace.notification_span("exit");
                        let ctx = Context::for_notification(
                            span,
                            self.client.clone(),
                            self.established_workspace(),
                        );
                        hook.invoke((Arc::clone(&self.state), ctx)).await;
                    }
                    // The LSP exit code comes from protocol-owned lifecycle
                    // state: 0 only when `shutdown` completed first.
                    let code = match self.lifecycle {
                        Lifecycle::ShuttingDown => 0,
                        _ => 1,
                    };
                    return Flow::Close(CloseCause::Exit { code });
                }
                "$/cancelRequest" => {
                    let bytes: &[u8] = if params.is_empty() { b"{}" } else { &params };
                    match serde_json::from_slice::<CancelParams>(bytes) {
                        Ok(cancel) => {
                            if let Some(reservation) = self.inbound.claim_cancellation(&cancel.id) {
                                enqueue_encoded(
                                    &self.out_tx,
                                    reservation.id,
                                    Err(LspError::RequestCancelled),
                                );
                            }
                        }
                        Err(error) => {
                            debug!(%error, "ignoring malformed $/cancelRequest");
                        }
                    }
                }
                "initialized" => {
                    // The initialized hook runs at most once, and only after a
                    // successful initialize transaction: outside the running
                    // state there is no Workspace for its Context, and the
                    // notification is ignored without consuming the hook, so a
                    // later, valid `initialized` still runs it. The params are
                    // decoded before the hook is taken, so a malformed
                    // notification leaves it in place too.
                    let Lifecycle::Running(_) = &self.lifecycle else {
                        debug!("initialized notification outside the running state ignored");
                        return Flow::Continue;
                    };
                    let params = match decode_initialized_params(&params) {
                        Ok(params) => params,
                        Err(error) => {
                            warn!(%error, "dropping initialized notification with malformed params");
                            return Flow::Continue;
                        }
                    };
                    let Some(hook) = self.on_initialized.take() else {
                        return Flow::Continue;
                    };
                    let span = self.trace.notification_span("initialized");
                    let ctx = Context::for_notification(
                        span,
                        self.client.clone(),
                        self.established_workspace(),
                    );
                    hook.invoke((Arc::clone(&self.state), ctx, params)).await;
                }
                other => {
                    // Outside the running state only the lifecycle and
                    // completion notifications handled above are processed:
                    // before `initialize` there is no Router, and after
                    // `shutdown` the connection accepts no further user work.
                    let Lifecycle::Running(service) = &self.lifecycle else {
                        debug!(method = other, "notification outside running state ignored");
                        return Flow::Continue;
                    };
                    let service = Arc::clone(service);

                    // A protocol-owned notification is a built-in (ADR 0018):
                    // its validation and any mutation run here, on the
                    // read-loop, before anything user-registered is reached, so
                    // the hook below — and every later message — observes the
                    // mutated state. A failure reports the notification
                    // error and skips the hook, leaving the connection to
                    // process the next message; a built-in may also skip its
                    // own hook after logging a non-error rejection at debug
                    // level (the work-done progress cancel built-in).
                    if let Some(built_in) = ProtocolNotification::from_method(other) {
                        if !self.accepts_protocol_notification(built_in) {
                            debug!(
                                method = other,
                                "document-sync notification disabled and ignored"
                            );
                            return Flow::Continue;
                        }
                        match self.process_protocol_notification(built_in, &params) {
                            Ok(BuiltInGate::RunHook) => {}
                            Ok(BuiltInGate::SkipHook) => return Flow::Continue,
                            Err(error) => {
                                let category = if crate::documents::is_capacity_error(&error) {
                                    ConnectionFailureCategory::Overload
                                } else {
                                    ConnectionFailureCategory::Protocol
                                };
                                self.failure_reporter.report(
                                    category,
                                    Some(ConnectionDirection::Inbound),
                                    Some(other),
                                    None,
                                );
                                warn!(method = other, %error, "protocol validation skipped its hook");
                                return Flow::Continue;
                            }
                        }
                    }

                    // The same bytes decode again into the method-erased value
                    // that crosses the Service stack. For a built-in this
                    // cannot fail — its typed decode above already succeeded.
                    let params = match decode_value(&params) {
                        Ok(params) => params,
                        Err(error) => {
                            self.failure_reporter.report(
                                ConnectionFailureCategory::Protocol,
                                Some(ConnectionDirection::Inbound),
                                Some(other),
                                None,
                            );
                            debug!(method = other, %error, "notification params ignored");
                            return Flow::Continue;
                        }
                    };
                    // A registered notification — a custom route or a built-in's
                    // post-validation hook — dispatches with no response; an
                    // unregistered one is ignored.
                    self.dispatch_notification(service, other, params).await;
                }
            },
            RawMessage::Response { id, result } => {
                // Only positive numeric IDs are allocated by `OutboundRegistry`.
                let id_num = match &id {
                    RequestId::Number(n) if *n > 0 => Some(*n as u32),
                    _ => None,
                };
                let delivered =
                    id_num.is_some_and(|n| self.client.outbound_registry().complete(n, result));
                if !delivered {
                    debug!(?id, "ignoring response with unknown or non-numeric id");
                }
            }
            RawMessage::ProtocolError { error } => {
                self.failure_reporter.report(
                    ConnectionFailureCategory::Protocol,
                    Some(ConnectionDirection::Inbound),
                    None,
                    None,
                );
                let _ = self
                    .out_tx
                    .send_required(RawMessage::ProtocolError { error });
            }
        }

        Flow::Continue
    }

    /// Run one normalized user notification through the Service stack.
    ///
    /// Takes `&mut self` like the rest of dispatch: the read-loop holds the
    /// engine exclusively across this await, which is what keeps a built-in's
    /// mutation and its hook one serial step.
    async fn dispatch_notification(
        &mut self,
        service: UserService<S>,
        method: &str,
        params: serde_json::Value,
    ) {
        let span = self.trace.notification_span(method);
        let ctx =
            Context::for_notification(span, self.client.clone(), self.established_workspace());
        let result = service
            .call(IncomingCall::notification(
                method.to_string(),
                params,
                ctx,
                Arc::clone(&self.state),
            ))
            .await;
        if !matches!(result, ServiceResult::NoResponse) {
            warn!("notification service attempted to produce a response");
        }
    }

    /// Validate and, where applicable, mutate for a protocol-owned notification.
    ///
    /// Built-in validation is what the documents themselves can establish: a
    /// change names a document that must already be open, and each of its
    /// ranges must be applicable under the negotiated encoding. Returning `Err`
    /// is what skips the notification's hook, so nothing partial is left for a
    /// hook to observe: a rejected `didChange` batch leaves the document at the
    /// revision the last accepted notification produced. The returned gate says
    /// whether the hook runs — the work-done progress cancel built-in skips it
    /// for malformed params without an error worth a warning.
    fn process_protocol_notification(
        &self,
        built_in: ProtocolNotification,
        raw_params: &Bytes,
    ) -> std::result::Result<BuiltInGate, LspError> {
        match built_in {
            ProtocolNotification::Open => {
                let params: DidOpenTextDocumentParams = decode_params(raw_params)?;
                self.documents.open(params.text_document)?;
            }
            ProtocolNotification::Change => {
                let params: DidChangeTextDocumentParams = decode_params(raw_params)?;
                validate_sync_changes(
                    self.document_sync
                        .change
                        .unwrap_or(TextDocumentSyncKind::INCREMENTAL),
                    &params.content_changes,
                )?;
                self.documents.apply_changes(
                    &params.text_document.uri,
                    params.text_document.version,
                    params.content_changes,
                )?;
            }
            ProtocolNotification::Close => {
                let params: DidCloseTextDocumentParams = decode_params(raw_params)?;
                // Closing a document that was never opened breaks the LSP's
                // ordering, but there is nothing to roll back and no response
                // to carry a complaint. The hook still runs: it observes the
                // same absence a real close would have left behind.
                if self.documents.close(&params.text_document.uri).is_none() {
                    debug!(
                        uri = ?params.text_document.uri,
                        "closing a document that was not open"
                    );
                }
            }
            ProtocolNotification::WillSave => {
                let _: WillSaveTextDocumentParams = decode_params(raw_params)?;
            }
            ProtocolNotification::Save => {
                let params: DidSaveTextDocumentParams = decode_params(raw_params)?;
                if matches!(
                    &self.document_sync.save,
                    Some(TextDocumentSyncSaveOptions::SaveOptions(options))
                        if options.include_text == Some(true)
                ) && params.text.is_none()
                {
                    return Err(LspError::invalid_request(
                        "didSave text is required by textDocumentSync.save.includeText",
                    ));
                }
            }
            ProtocolNotification::WorkspaceFolders => {
                let params: DidChangeWorkspaceFoldersParams = decode_params(raw_params)?;
                self.established_workspace().apply_folder_change(params);
            }
            ProtocolNotification::Configuration => {
                let params: DidChangeConfigurationParams = decode_params(raw_params)?;
                self.established_workspace()
                    .set_configuration(params.settings);
            }
            ProtocolNotification::Trace => {
                let params: SetTraceParams = decode_params(raw_params)?;
                self.established_workspace().set_trace(params.value);
            }
            ProtocolNotification::ProgressCancel => {
                return Ok(gate_progress_cancel(
                    self.client.progress_registry(),
                    raw_params,
                ));
            }
        }
        Ok(BuiltInGate::RunHook)
    }

    fn accepts_protocol_notification(&self, built_in: ProtocolNotification) -> bool {
        match built_in {
            ProtocolNotification::WorkspaceFolders
            | ProtocolNotification::Configuration
            | ProtocolNotification::Trace
            | ProtocolNotification::ProgressCancel => true,
            _ if self.document_sync.change == Some(TextDocumentSyncKind::NONE) => false,
            ProtocolNotification::Open | ProtocolNotification::Close => {
                self.document_sync.open_close == Some(true)
            }
            ProtocolNotification::Change => matches!(
                self.document_sync.change,
                Some(TextDocumentSyncKind::FULL | TextDocumentSyncKind::INCREMENTAL)
            ),
            ProtocolNotification::WillSave => self.document_sync.will_save == Some(true),
            ProtocolNotification::Save => matches!(
                self.document_sync.save,
                Some(TextDocumentSyncSaveOptions::Supported(true))
                    | Some(TextDocumentSyncSaveOptions::SaveOptions(_))
            ),
        }
    }

    /// Run the one `initialize` transaction (ADR 0017, ADR 0018).
    ///
    /// In order: validate and consume the sole `initialize`; run
    /// `configure_initialize` against a transactional registrar; on success
    /// commit and permanently freeze the Router; establish the `Workspace`,
    /// `Documents` encoding, and generated capabilities; park the
    /// `on_initialized`, `on_shutdown`, and `on_exit` hooks for the running state; run
    /// `on_initialize` for optional `ServerInfo`; then enter the running state
    /// and reply. Any configuration, validation, or `on_initialize` failure
    /// enqueues the fixed error and requests the terminal close rather than
    /// returning to uninitialized.
    async fn initialize(&mut self, span: &Span, reservation: Reservation, params: Bytes) -> Flow {
        // A second `initialize` after the transaction has run is invalid.
        if !matches!(self.lifecycle, Lifecycle::Uninitialized(_)) {
            self.inbound.complete(
                &self.out_tx,
                reservation,
                Err(LspError::ServerError {
                    code: -32600,
                    message: "server already initialized".into(),
                    data: None,
                }),
            );
            return Flow::Continue;
        }

        // Malformed `initialize` params leave the transaction unspent: the
        // client may retry with a valid request, so stay uninitialized.
        let params = match decode_params::<InitializeParams>(&params) {
            Ok(params) => params,
            Err(err) => {
                self.failure_reporter.report(
                    ConnectionFailureCategory::Protocol,
                    Some(ConnectionDirection::Inbound),
                    Some("initialize"),
                    Some(&reservation.id),
                );
                self.inbound.complete(&self.out_tx, reservation, Err(err));
                return Flow::Continue;
            }
        };

        // Take ownership of the pending registrations and callbacks; the
        // transaction consumes them exactly once.
        let pending = match std::mem::replace(&mut self.lifecycle, Lifecycle::Initializing) {
            Lifecycle::Uninitialized(pending) => *pending,
            // The `matches!` guard above already established this arm.
            _ => unreachable!("initialize runs only while uninitialized"),
        };
        let Pending {
            registrations,
            file_provider,
            configure_initialize,
            on_initialize,
            on_initialized,
            on_shutdown,
            on_exit,
            layers,
            concurrency_limit,
        } = pending;

        // Run the conditional registration transaction against a registrar
        // seeded with all static registrations. A callback error or any
        // combined-validation conflict discards the whole transaction — the
        // registrar (and every static and conditional registration in it) is
        // dropped, so nothing partial leaks.
        let mut registrar = InitializeRegistrar::new(registrations);
        let committed = match configure_initialize {
            Some(callback) => callback.invoke(&params, &mut registrar),
            None => Ok(()),
        }
        .and_then(|()| registrar.commit().map_err(LspError::internal));

        let registrations = match committed {
            Ok(registrations) => registrations,
            Err(_err) => {
                // ADR 0017's fixed error: configuration or combined-validation
                // failure reports InternalError and enters the close path.
                self.inbound.complete(
                    &self.out_tx,
                    reservation,
                    Err(LspError::internal("initialization failed")),
                );
                return Flow::Close(CloseCause::InitializeFailed);
            }
        };

        // Commit: permanently freeze the Router before any capability is
        // generated.
        let router = Arc::new(registrations.freeze());

        // The post-initialize lifecycle hooks become reachable only through
        // dispatch in the running state, which the transaction enters at its
        // end; they stay parked here until then.
        self.on_initialized = on_initialized;
        self.on_shutdown = on_shutdown;
        self.on_exit = on_exit;

        // Establish Workspace, Documents encoding, and generated capabilities
        // from InitializeParams before `on_initialize` observes them. Per
        // ADR 0018's precedence, the Workspace is established (step 4) before
        // protocol-owned fields are negotiated and capabilities generated
        // (step 5). The Workspace takes ownership of the connection's
        // Documents handle; the engine keeps its own clone for the built-in
        // document-sync mutations.
        let established = Workspace::from_params_with_provider(
            &params,
            self.documents.clone(),
            file_provider,
            self.client.shared_trace(),
        );
        self.workspace = Some(established.clone());

        let position_encoding = self.documents.negotiate_position_encoding(&params);
        let mut capabilities = router.generated_capabilities();
        let standard_capabilities = capabilities.standard_mut();
        standard_capabilities.position_encoding = Some(position_encoding);
        // Document sync is a protocol built-in rather than a registration
        // (ADR 0018): the engine applies every `didOpen`, `didChange`, and
        // `didClose` itself. So it advertises the sync kind those built-ins
        // implement, as one more protocol-owned field layered onto the frozen
        // catalog (ADR 0017) beside the negotiated position encoding. A client
        // that sees no `textDocumentSync` sends no document notification at
        // all, leaving the built-ins and every post-validation hook unreachable.
        // Nothing user-registered contributes this field, so there is no
        // contribution here to overwrite.
        let document_sync = router.document_sync();
        self.document_sync = document_sync.options;
        standard_capabilities.text_document_sync = Some(document_sync.capability);
        // Workspace-folder sync is likewise a protocol built-in, so the
        // engine advertises its support itself. Registration-contributed
        // workspace fields (the file-operation families) come from the frozen
        // catalog and are preserved beside the protocol-owned field.
        let file_operations = standard_capabilities
            .workspace
            .take()
            .and_then(|workspace| workspace.file_operations);
        standard_capabilities.workspace = Some(WorkspaceServerCapabilities {
            workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                supported: Some(true),
                change_notifications: Some(OneOf::Left(true)),
            }),
            file_operations,
        });

        // `on_initialize` may contribute optional ServerInfo but cannot
        // register routes or replace the generated capabilities.
        let server_info = match on_initialize {
            Some(hook) => {
                let ctx = Context::for_request(
                    reservation.id.clone(),
                    span.clone(),
                    self.client.clone(),
                    established,
                );
                match hook
                    .invoke((
                        Arc::clone(&self.state),
                        ctx,
                        params,
                        self.session.child_token(),
                    ))
                    .instrument(span.clone())
                    .await
                {
                    Ok(server_info) => server_info,
                    Err(err) => {
                        // ADR 0018: on_initialize failure sends that error, then
                        // enters the close path; the frozen Router and
                        // established Workspace are never exposed to later
                        // dispatch.
                        self.inbound.complete(&self.out_tx, reservation, Err(err));
                        return Flow::Close(CloseCause::InitializeFailed);
                    }
                }
            }
            None => None,
        };

        self.inbound.complete(
            &self.out_tx,
            reservation,
            encode_body(&WireInitializeResult {
                capabilities,
                server_info,
            }),
        );
        self.lifecycle = Lifecycle::Running(build_service_stack(
            router,
            layers,
            concurrency_limit,
            self.failure_reporter.clone(),
        ));
        Flow::Continue
    }

    /// The established [`Workspace`]. Dispatch reaches user code only in the
    /// running state, which the initialize transaction enters only after
    /// establishing the Workspace, so it is always present here.
    fn established_workspace(&self) -> Workspace {
        self.workspace.clone().expect(
            "user dispatch runs only after the initialize transaction establishes the workspace",
        )
    }

    /// Spawn one user request into the engine's task group. The task races user
    /// dispatch against its explicit cancellation token. When cancellation
    /// wins, one final poll lets cooperative handler code observe the token
    /// before the future is dropped; the completion gate rejects any result
    /// produced after another path claimed the reservation.
    fn spawn_service_request(
        &mut self,
        service: UserService<S>,
        span: Span,
        reservation: Reservation,
        method: String,
        params: serde_json::Value,
        cancellation: CancellationToken,
        default_timeout: Duration,
    ) {
        let state = Arc::clone(&self.state);
        let workspace = self.established_workspace();
        let out_tx = self.out_tx.clone();
        let client = self.client.clone();
        let inbound = self.inbound.clone();
        let permit = Arc::clone(&reservation._permit);
        let trace = self.trace;
        self.tasks.spawn(
            async move {
                let id = reservation.id.clone();
                let trace_id = id.clone();
                let trace_method = method.clone();
                let cancellation_for_handler = cancellation.clone();
                let ctx = Context::for_request(id.clone(), span, client, workspace)
                    .with_cancellation(cancellation_for_handler);
                let handler_timeout = HandlerTimeout::new(
                    default_timeout,
                    trace,
                    trace_method.clone(),
                    trace_id.clone(),
                );
                let call =
                    IncomingCall::request(method, id, params, ctx, state, handler_timeout.clone());
                let handler = service.call(call);
                let completion = select(Box::pin(handler), Box::pin(cancellation.cancelled()));
                let result = match select(
                    Box::pin(completion),
                    Box::pin(handler_timeout.wait_until_armed()),
                )
                .await
                {
                    Either::Left((Either::Left((result, _)), _)) => result,
                    Either::Left((Either::Right(((), handler)), _)) => {
                        cooperatively_cancelled_result(handler)
                    }
                    Either::Right(((), completion)) => {
                        match select(
                            Box::pin(completion),
                            Box::pin(crate::runtime::sleep(handler_timeout.get())),
                        )
                        .await
                        {
                            Either::Left((Either::Left((result, _)), _)) => result,
                            Either::Left((Either::Right(((), handler)), _)) => {
                                cooperatively_cancelled_result(handler)
                            }
                            Either::Right(((), completion)) => {
                                handler_timeout.finish(DeadlineAction::Expired);
                                cancellation.cancel();
                                // Match peer cancellation's cooperative final poll: code
                                // awaiting the token observes expiry before its future is
                                // dropped, while the timeout remains the selected result.
                                let _ = completion.now_or_never();
                                ServiceResult::Error(LspError::ServerError {
                                    code: SERVER_CANCELLED as i32,
                                    message: HANDLER_DEADLINE_EXPIRED.to_string(),
                                    data: None,
                                })
                            }
                        }
                    }
                };
                handler_timeout.finish(match &result {
                    ServiceResult::Error(LspError::RequestCancelled) => DeadlineAction::Cancelled,
                    ServiceResult::Error(LspError::ServerError { message, .. })
                        if message == HANDLER_DEADLINE_EXPIRED =>
                    {
                        DeadlineAction::Expired
                    }
                    _ => DeadlineAction::Completed,
                });
                let result = match result {
                    ServiceResult::Response(value) => encode_body(&value),
                    ServiceResult::Error(error) => Err(error),
                    ServiceResult::NoResponse => {
                        Err(LspError::internal("request service returned no response"))
                    }
                };
                inbound.complete(&out_tx, reservation, result);
            },
            permit,
        );
    }

    /// The engine's one close operation (ADR 0018).
    ///
    /// Every close cause runs exactly these steps, in this order, and a second
    /// call is a no-op: new outbound work is rejected, the session is
    /// cancelled, every pending `Client` request is resolved, the inbound,
    /// outbound, and progress registries are emptied, every handler task is
    /// aborted and then joined, and the outbound queue is closed before the
    /// writer task is joined. No task is detached and no pending `Client`
    /// future is left unresolved.
    async fn close(&mut self) {
        if matches!(self.lifecycle, Lifecycle::Exited) {
            return;
        }
        self.lifecycle = Lifecycle::Exited;
        self.client.close_connection();
        self.session.cancel();
        // Complete all pending outbound requests before cancelling inbound
        // tasks, so handler futures awaiting a client response observe
        // `ClientError::Cancelled`, allowing them to unblock and exit cleanly.
        self.client.outbound_registry().close_all();
        self.inbound.close_all();
        // The progress registry is connection state too: clearing it leaves no
        // stale tokens behind, so a handle that outlives the session observes
        // `ProgressError::UnknownToken` rather than a still-active token. Each
        // connection owns its registry through its own `Client`, so clearing
        // cannot touch another connection.
        self.client.progress_registry().clear();
        self.tasks.abort_and_join().await;
        // A DocumentsView may outlive its Context when user code retains the
        // cloneable handle. Empty the shared store explicitly so connection
        // shutdown releases every snapshot and its count/byte accounting even
        // while such a view remains alive.
        self.documents.clear();
        // Closing the queue is the writer's stop signal: it drains what is
        // already enqueued, shuts the writer half down, and ends. Joining it
        // rather than aborting it is what lets those last responses reach the
        // peer, and joining is what keeps it from being detached.
        self.client.close_outbound();
        if let Some(send_task) = self.send_task.take() {
            send_task.join().await;
        }
    }
}

/// Serving normally ends through [`ProtocolEngine::close`], which has already
/// joined every task by the time the engine drops. Dropping the serve future
/// before that — the caller abandoning the connection — leaves no one able to
/// join them, so abort here rather than detach a task that would keep running
/// against a connection nobody owns.
impl<S, R> Drop for ProtocolEngine<S, R> {
    fn drop(&mut self) {
        for task in &self.tasks.handles {
            task.handle.abort();
        }
        if let Some(send_task) = &self.send_task {
            send_task.abort();
        }
    }
}

enum Flow {
    Continue,
    /// A terminal path — `exit`, or the close a failed initialize transaction
    /// enters (ADR 0018) once its fixed error is enqueued — requesting the
    /// engine's one close operation with the cause that reached it.
    Close(CloseCause),
}

/// Select the reported cause after close has quiesced every task that could
/// fail a required outbound admission (ADR 0026).
fn final_close_cause(close: &CloseSignal, out_tx: &OutboundQueue) -> CloseCause {
    let recorded = close
        .take_cause()
        .expect("every path out of the read-loop records its close cause");
    if out_tx.failure().is_cancelled() {
        CloseCause::WriterFailed
    } else {
        recorded
    }
}

/// Enqueue a success response after the protocol engine's final wire encoding,
/// or enqueue the mapped wire error.
fn enqueue_encoded(
    out_tx: &OutboundQueue,
    id: RequestId,
    result: std::result::Result<Bytes, LspError>,
) {
    let response = match result {
        Ok(bytes) => RawMessage::Response {
            id,
            result: Ok(bytes),
        },
        Err(err) => error_response(id, &err),
    };
    let _ = out_tx.send_required(response);
}

fn completion_kind(result: &std::result::Result<Bytes, LspError>) -> Completion {
    match result {
        Ok(_) => Completion::Success,
        Err(LspError::RequestCancelled) => Completion::Cancelled,
        Err(LspError::ServerError { message, .. }) if message == HANDLER_DEADLINE_EXPIRED => {
            Completion::DeadlineExpired
        }
        Err(_) => Completion::Error,
    }
}

fn error_response(id: RequestId, err: &LspError) -> RawMessage {
    RawMessage::Response {
        id,
        result: Err(JsonRpcError {
            code: err.code(),
            message: err.message(),
            data: err.data().cloned(),
        }),
    }
}

fn enqueue_error(out_tx: &OutboundQueue, id: RequestId, err: LspError) {
    let _ = out_tx.send_required(error_response(id, &err));
}

fn cooperatively_cancelled_result<F>(handler: F) -> ServiceResult
where
    F: Future<Output = ServiceResult>,
{
    // CancellationToken wakes every waiter, but an executor yield does not
    // guarantee this handler is polled before a separate abort. Poll it here,
    // in its own task, then finish without relying on scheduler fairness.
    let _ = handler.now_or_never();
    ServiceResult::Error(LspError::RequestCancelled)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use lsp_types::ProgressToken;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    /// Run `gate_progress_cancel` against `registry` with `raw_params`,
    /// capturing every tracing event the call emits on this thread.
    fn gated_cancel(
        registry: &ProgressRegistry,
        raw_params: &'static [u8],
    ) -> (BuiltInGate, crate::test_util::EventCapture) {
        let _capture = crate::test_util::tracing_capture_lock();
        let events = crate::test_util::EventCapture::new();
        let subscriber = tracing_subscriber::registry().with(events.clone());
        let gate = tracing::subscriber::with_default(subscriber, || {
            gate_progress_cancel(registry, &Bytes::from_static(raw_params))
        });
        (gate, events)
    }

    #[test]
    fn a_matching_cancellable_token_fires_and_opens_the_hook_gate() {
        let registry = ProgressRegistry::default();
        let cancellation = CancellationToken::new();
        registry.register(ProgressToken::Number(1), true, cancellation.clone());

        let (gate, events) = gated_cancel(&registry, br#"{"token": 1}"#);

        assert!(
            matches!(gate, BuiltInGate::RunHook),
            "a successful decode lets the hook run"
        );
        assert!(cancellation.is_cancelled());
        assert!(
            registry.is_active(&ProgressToken::Number(1)),
            "cancellation never ends the progress: the token stays registered"
        );
        assert!(
            events.messages().is_empty(),
            "a matched cancel logs nothing, got {:?}",
            events.messages()
        );
    }

    #[test]
    fn non_cancellable_and_inactive_tokens_log_at_debug_and_keep_the_gate_open() {
        let registry = ProgressRegistry::default();
        let plain = CancellationToken::new();
        registry.register(ProgressToken::Number(1), false, plain.clone());

        let (gate, events) = gated_cancel(&registry, br#"{"token": 1}"#);
        assert!(matches!(gate, BuiltInGate::RunHook));
        assert!(!plain.is_cancelled(), "a non-cancellable token never fires");
        assert!(
            events.contains_at(tracing::Level::DEBUG, "non-cancellable token"),
            "got {:?}",
            events.messages()
        );

        let (gate, events) = gated_cancel(&registry, br#"{"token": 99}"#);
        assert!(matches!(gate, BuiltInGate::RunHook));
        assert!(
            events.contains_at(tracing::Level::DEBUG, "unknown or ended token"),
            "got {:?}",
            events.messages()
        );
        assert!(
            registry.is_active(&ProgressToken::Number(1)),
            "an unknown token leaves the registry untouched"
        );
    }

    #[test]
    fn malformed_cancel_params_log_at_debug_and_close_the_hook_gate() {
        let registry = ProgressRegistry::default();
        let cancellation = CancellationToken::new();
        registry.register(ProgressToken::Number(1), true, cancellation.clone());

        for raw in [
            br#"{"token": true}"#.as_slice(),
            br#"{}"#.as_slice(),
            b"not json".as_slice(),
        ] {
            let (gate, events) = gated_cancel(&registry, raw);
            assert!(
                matches!(gate, BuiltInGate::SkipHook),
                "malformed params {raw:?} skip the hook"
            );
            assert!(
                events.contains_at(
                    tracing::Level::DEBUG,
                    "malformed window/workDoneProgress/cancel"
                ),
                "got {:?}",
                events.messages()
            );
        }
        assert!(
            !cancellation.is_cancelled(),
            "malformed params never cancel anything"
        );
    }

    fn content_change(with_range: bool) -> lsp_types::TextDocumentContentChangeEvent {
        lsp_types::TextDocumentContentChangeEvent {
            range: with_range.then(|| lsp_types::Range {
                start: lsp_types::Position::new(0, 0),
                end: lsp_types::Position::new(0, 1),
            }),
            range_length: None,
            text: "replacement".to_string(),
        }
    }

    #[test]
    fn sync_kind_validation_accepts_only_compatible_change_shapes() {
        assert!(
            validate_sync_changes(TextDocumentSyncKind::FULL, &[content_change(false)]).is_ok()
        );
        assert!(
            validate_sync_changes(TextDocumentSyncKind::FULL, &[content_change(true)]).is_err()
        );
        assert!(
            validate_sync_changes(
                TextDocumentSyncKind::INCREMENTAL,
                &[content_change(true), content_change(false)],
            )
            .is_ok()
        );
        assert!(
            validate_sync_changes(TextDocumentSyncKind::NONE, &[content_change(false)]).is_err()
        );
    }

    #[test]
    fn the_first_requester_records_the_cause_and_later_ones_do_not_replace_it() {
        let close = CloseSignal::new();
        assert!(!close.requested().is_cancelled());

        close.request(CloseCause::WriterFailed);
        close.request(CloseCause::Exit { code: 0 });
        close.request(CloseCause::ReaderEof);

        assert!(
            close.requested().is_cancelled(),
            "requesting close wakes the read-loop"
        );
        assert!(
            matches!(close.take_cause(), Some(CloseCause::WriterFailed)),
            "the first cause requested is the one reported"
        );
        assert!(
            close.take_cause().is_none(),
            "the cause is taken once, by the read-loop that ran the close"
        );
    }

    #[test]
    fn a_late_required_enqueue_failure_overrides_an_earlier_close_cause() {
        let message = RawMessage::Notification {
            method: "test/required".into(),
            params: Bytes::new(),
        };
        let (queue, _rx) = OutboundQueue::bounded(1, usize::MAX);
        queue.send(message.clone()).unwrap();
        let close = CloseSignal::new();
        close.request(CloseCause::InitializeFailed);

        assert!(queue.send_required(message).is_err());

        assert!(matches!(
            final_close_cause(&close, &queue),
            CloseCause::WriterFailed
        ));
    }

    #[test]
    fn every_cause_maps_to_one_outcome_or_a_transport_error() {
        assert_eq!(
            CloseCause::Exit { code: 0 }.into_result().unwrap(),
            Outcome::Exit { code: 0 }
        );
        assert_eq!(
            CloseCause::ReaderEof.into_result().unwrap(),
            Outcome::TransportClosed
        );
        assert_eq!(
            CloseCause::WriterFailed.into_result().unwrap(),
            Outcome::WriterFailed
        );
        assert_eq!(
            CloseCause::InitializeFailed.into_result().unwrap(),
            Outcome::InitializeFailed
        );
        assert!(matches!(
            CloseCause::ReaderFailed(TransportError::Malformed("bad".into())).into_result(),
            Err(Error::Transport(_))
        ));
    }

    /// A peer may reuse a request ID once the previous request under it has
    /// been answered. The completion gate is scoped to the reservation, not the
    /// ID, so the first request's task cannot answer the second request when it
    /// finishes after its own entry was claimed.
    #[test]
    fn a_stale_reservation_cannot_claim_a_reused_request_id() {
        let (out_tx, mut out_rx) = OutboundQueue::new(crate::DEFAULT_OUTBOUND_WARNING_THRESHOLD);
        let registry = InboundRegistry::new(2);
        let id = RequestId::Number(2);
        let session = CancellationToken::new();

        let first = registry
            .reserve(id.clone(), Some(&session))
            .expect("the id is free")
            .reservation;
        assert!(
            matches!(
                registry.reserve(id.clone(), Some(&session)),
                Err(InboundReserveError::DuplicateId)
            ),
            "an in-flight id is not reserved twice"
        );

        // `$/cancelRequest` claims the gate and answers the first request.
        let cancelled = registry
            .claim_cancellation(&id)
            .expect("the first request is cancellable");
        enqueue_encoded(&out_tx, cancelled.id, Err(LspError::RequestCancelled));
        // The peer then reuses the id for a new request.
        let second = registry
            .reserve(id.clone(), Some(&session))
            .expect("the id is free once the first request is answered")
            .reservation;

        // The first request's task only now produces a result.
        registry.complete(&out_tx, first, encode_body(&"race"));
        registry.complete(&out_tx, second, encode_body(&"reused"));

        assert_eq!(
            out_rx.try_recv().unwrap().id(),
            Some(&id),
            "the cancellation answers the first request"
        );
        let answer = out_rx.try_recv().expect("the second request is answered");
        match answer {
            RawMessage::Response {
                result: Ok(body), ..
            } => assert_eq!(
                serde_json::from_slice::<String>(&body).unwrap(),
                "reused",
                "the second request gets its own result, not the stale one"
            ),
            other => panic!("expected a success response, got {other:?}"),
        }
        assert!(
            out_rx.try_recv().is_err(),
            "the stale reservation enqueued nothing"
        );
    }

    #[test]
    fn an_exhausted_registry_stays_bounded_and_cancellation_releases_its_entry() {
        let registry = InboundRegistry::new(1);
        let session = CancellationToken::new();
        let accepted = registry
            .reserve(RequestId::Number(2), Some(&session))
            .expect("the one slot is available");

        for id in 3..=66 {
            assert!(matches!(
                registry.reserve(RequestId::Number(id), Some(&session)),
                Err(InboundReserveError::CapacityExhausted)
            ));
        }
        assert_eq!(
            registry.inner.lock().unwrap().entries.len(),
            1,
            "the flood retained only the admitted registry entry"
        );

        let cancelled = registry
            .claim_cancellation(&RequestId::Number(2))
            .expect("the admitted request is cancellable");
        assert!(
            registry.inner.lock().unwrap().entries.is_empty(),
            "cancellation releases the registry entry"
        );
        drop(cancelled);
        assert!(matches!(
            registry.reserve(RequestId::Number(67), Some(&session)),
            Err(InboundReserveError::CapacityExhausted)
        ));
        drop(accepted);
        assert!(
            registry
                .reserve(RequestId::Number(67), Some(&session))
                .is_ok(),
            "capacity returns after the admitted task drops its reservation"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn a_finished_task_holds_capacity_until_its_handle_is_reaped() {
        let registry = InboundRegistry::new(1);
        let session = CancellationToken::new();
        let accepted = registry
            .reserve(RequestId::Number(2), Some(&session))
            .expect("the one slot is available");
        let permit = Arc::clone(&accepted.reservation._permit);
        let (out_tx, _out_rx) = OutboundQueue::new(crate::DEFAULT_OUTBOUND_WARNING_THRESHOLD);
        registry.complete(&out_tx, accepted.reservation, encode_body(&"done"));

        let mut tasks = TaskGroup::new(default_runtime());
        tasks.spawn(async {}, permit);
        while !tasks.handles[0].handle.is_finished() {
            tasks.runtime.yield_now().await;
        }
        assert_eq!(tasks.handles.len(), 1, "the finished handle is still owned");
        assert!(matches!(
            registry.reserve(RequestId::Number(3), Some(&session)),
            Err(InboundReserveError::CapacityExhausted)
        ));

        tasks.reap_finished().await;
        assert!(tasks.handles.is_empty(), "the finished handle was reaped");
        assert!(
            registry
                .reserve(RequestId::Number(3), Some(&session))
                .is_ok(),
            "reaping the handle releases its admission permit"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn aborting_the_task_group_releases_disconnect_capacity() {
        let registry = InboundRegistry::new(1);
        let session = CancellationToken::new();
        let accepted = registry
            .reserve(RequestId::Number(2), Some(&session))
            .expect("the one slot is available");
        let permit = Arc::clone(&accepted.reservation._permit);
        let mut tasks = TaskGroup::new(default_runtime());
        tasks.spawn(std::future::pending(), permit);

        registry.close_all();
        drop(accepted);
        assert!(registry.inner.lock().unwrap().entries.is_empty());
        assert!(matches!(
            registry.reserve(RequestId::Number(3), Some(&session)),
            Err(InboundReserveError::CapacityExhausted)
        ));

        tasks.abort_and_join().await;
        assert!(tasks.handles.is_empty(), "disconnect joined every task");
        assert!(
            registry
                .reserve(RequestId::Number(3), Some(&session))
                .is_ok(),
            "joining aborted tasks releases their admission permits"
        );
    }

    #[test]
    fn only_a_shutdown_exit_reports_code_zero() {
        assert_eq!(Outcome::Exit { code: 0 }.code(), 0);
        assert_eq!(Outcome::Exit { code: 1 }.code(), 1);
        assert_eq!(Outcome::TransportClosed.code(), 1);
        assert_eq!(Outcome::WriterFailed.code(), 1);
        assert_eq!(Outcome::InitializeFailed.code(), 1);
    }

    // --- Runtime presence detection -------------------------------------------

    /// A transport that never starts: the runtime check fails before `split`
    /// is reached, so its halves only need to exist.
    struct DetectRuntimeTransport;

    impl Transport for DetectRuntimeTransport {
        type Reader = DetectRuntimeReader;
        type Writer = DetectRuntimeWriter;

        fn split(self) -> (Self::Reader, Self::Writer) {
            (DetectRuntimeReader, DetectRuntimeWriter)
        }
    }

    struct DetectRuntimeReader;

    impl TransportReader for DetectRuntimeReader {
        async fn recv(&mut self) -> std::result::Result<RawMessage, TransportError> {
            Err(TransportError::Closed)
        }
    }

    struct DetectRuntimeWriter;

    impl TransportWriter for DetectRuntimeWriter {
        async fn send(&mut self, _msg: RawMessage) -> std::result::Result<(), TransportError> {
            Ok(())
        }

        async fn shutdown(self) -> std::result::Result<(), TransportError> {
            Ok(())
        }
    }

    #[test]
    fn serving_without_a_tokio_runtime_reports_the_missing_runtime() {
        // A plain `#[test]` runs on a thread without a Tokio runtime. The
        // runtime check precedes every await, so one poll reports the error.
        let server = Server::builder(()).build().expect("an empty server builds");
        let mut serving = Box::pin(server.serve(DetectRuntimeTransport));
        let waker = futures_util::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        let outcome = match std::future::Future::poll(serving.as_mut(), &mut cx) {
            std::task::Poll::Ready(outcome) => outcome,
            std::task::Poll::Pending => panic!("the missing runtime is reported on the first poll"),
        };

        assert!(
            matches!(outcome, Err(Error::RuntimeRequired)),
            "serving without a runtime reports the missing runtime, got {outcome:?}"
        );
    }

    // --- Outbound queue depth observability tests ----------------------------

    enum TestOutboundNotification {}

    impl lsp_types::notification::Notification for TestOutboundNotification {
        type Params = serde_json::Value;
        const METHOD: &'static str = "test/outbound-notification";
    }

    enum TestOutboundRequest {}

    impl lsp_types::request::Request for TestOutboundRequest {
        type Params = serde_json::Value;
        type Result = String;
        const METHOD: &'static str = "test/outbound-request";
    }

    fn send_loop_message(tag: u8) -> RawMessage {
        RawMessage::Notification {
            method: "test/send-loop".into(),
            params: Bytes::from(vec![tag]),
        }
    }

    fn encoded_len(message: &RawMessage) -> usize {
        crate::transport::envelope::serialize(message)
            .expect("the test message encodes")
            .len()
    }

    /// A writer that records what it sent and fails the `fail_on_send`-th send
    /// (1-based; `None` never fails), driving the send-loop through its
    /// success, draining, and terminal-failure paths.
    struct ScriptedWriter {
        outbox: Arc<Mutex<Vec<RawMessage>>>,
        fail_on_send: Option<usize>,
        sends: usize,
    }

    struct SlowWriter {
        started: Arc<tokio::sync::Notify>,
        releases: Arc<tokio::sync::Semaphore>,
    }

    impl TransportWriter for SlowWriter {
        async fn send(&mut self, _msg: RawMessage) -> std::result::Result<(), TransportError> {
            self.started.notify_one();
            self.releases
                .acquire()
                .await
                .expect("the test keeps the release gate open")
                .forget();
            Ok(())
        }

        async fn shutdown(self) -> std::result::Result<(), TransportError> {
            Ok(())
        }
    }

    impl TransportWriter for ScriptedWriter {
        async fn send(&mut self, msg: RawMessage) -> std::result::Result<(), TransportError> {
            self.sends += 1;
            if self.fail_on_send == Some(self.sends) {
                return Err(TransportError::Closed);
            }
            self.outbox.lock().unwrap().push(msg);
            Ok(())
        }

        async fn shutdown(self) -> std::result::Result<(), TransportError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn requests_responses_and_notifications_share_one_depth_counter() {
        let (queue, _rx) = OutboundQueue::new(crate::DEFAULT_OUTBOUND_WARNING_THRESHOLD);
        let client = Client::new(queue.clone(), OutboundRegistry::default(), None);

        client
            .notify::<TestOutboundNotification>(serde_json::json!({}))
            .unwrap();
        enqueue_encoded(
            &queue,
            RequestId::Number(1),
            Ok(Bytes::from_static(b"null")),
        );

        // The request future enqueues synchronously on its first poll, then
        // awaits the peer's response.
        let pending = client.request::<TestOutboundRequest>(serde_json::json!({}));
        futures_util::pin_mut!(pending);
        assert!(
            futures_util::poll!(pending.as_mut()).is_pending(),
            "the request awaits the peer's response"
        );
        assert_eq!(queue.depth(), 3);
        client
            .outbound_registry()
            .complete(1, Ok(Bytes::from_static(b"\"pong\"")));
        let answer = pending.await;
        assert_eq!(answer.unwrap(), "pong");

        assert_eq!(
            queue.depth(),
            3,
            "a notification, a response, and a request all increment the one counter"
        );
    }

    #[tokio::test]
    async fn writer_failure_releases_attempted_and_abandoned_accounting() {
        let (queue, rx) = OutboundQueue::new(16);
        let client = Client::new(queue.clone(), OutboundRegistry::default(), None);
        for tag in 0..3 {
            queue.send(send_loop_message(tag)).unwrap();
        }
        assert_eq!(queue.depth(), 3);

        let outbox = Arc::new(Mutex::new(Vec::new()));
        let writer = ScriptedWriter {
            outbox: outbox.clone(),
            fail_on_send: Some(2),
            sends: 0,
        };
        let close = CloseSignal::new();
        send_loop(writer, rx, client, close.clone()).await;

        assert_eq!(
            queue.depth(),
            0,
            "writer failure abandons every queued slot"
        );
        assert_eq!(
            queue.encoded_bytes(),
            0,
            "writer failure abandons every queued byte charge"
        );
        assert_eq!(
            outbox.lock().unwrap().len(),
            1,
            "only the first send landed on the transport"
        );
        assert!(
            matches!(close.take_cause(), Some(CloseCause::WriterFailed)),
            "the writer reports its terminal failure"
        );
    }

    #[tokio::test]
    async fn draining_after_close_decrements_every_message_in_order() {
        let (queue, rx) = OutboundQueue::new(2);
        let client = Client::new(queue.clone(), OutboundRegistry::default(), None);
        for tag in 0..3 {
            queue.send(send_loop_message(tag)).unwrap();
        }
        client.close_outbound();

        let outbox = Arc::new(Mutex::new(Vec::new()));
        let writer = ScriptedWriter {
            outbox: outbox.clone(),
            fail_on_send: None,
            sends: 0,
        };
        let close = CloseSignal::new();
        send_loop(writer, rx, client, close.clone()).await;

        assert_eq!(
            queue.depth(),
            0,
            "draining decrements every queued message, above and below the threshold"
        );
        assert_eq!(
            queue.encoded_bytes(),
            0,
            "close releases every encoded-byte charge after draining"
        );
        let tags: Vec<u8> = outbox
            .lock()
            .unwrap()
            .iter()
            .map(|msg| match msg {
                RawMessage::Notification { params, .. } => params[0],
                other => panic!("expected notifications, got {other:?}"),
            })
            .collect();
        assert_eq!(
            tags,
            vec![0, 1, 2],
            "draining never drops or reorders a message"
        );
        assert!(
            close.take_cause().is_none(),
            "clean draining is not a writer failure"
        );
    }

    #[tokio::test]
    async fn a_slow_reader_cannot_grow_count_or_bytes_past_the_policy() {
        let message_bytes = encoded_len(&send_loop_message(0));
        let (queue, rx) = OutboundQueue::bounded(2, message_bytes * 2);
        let client = Client::new(queue.clone(), OutboundRegistry::default(), None);
        queue.send(send_loop_message(0)).unwrap();
        queue.send(send_loop_message(1)).unwrap();

        let started = Arc::new(tokio::sync::Notify::new());
        let releases = Arc::new(tokio::sync::Semaphore::new(0));
        let writer = SlowWriter {
            started: started.clone(),
            releases: releases.clone(),
        };
        let close = CloseSignal::new();
        let serving = tokio::spawn(send_loop(writer, rx, client.clone(), close));

        started.notified().await;
        assert_eq!(queue.depth(), 2, "the in-flight write remains accounted");
        assert_eq!(queue.encoded_bytes(), message_bytes * 2);
        assert!(matches!(
            queue.send(send_loop_message(2)),
            Err(crate::client::OutboundSendError::Overloaded)
        ));
        assert_eq!(queue.depth(), 2, "the rejected send consumes no slot");
        assert_eq!(
            queue.encoded_bytes(),
            message_bytes * 2,
            "the rejected send consumes no bytes"
        );

        releases.add_permits(1);
        for _ in 0..100 {
            if queue.depth() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(queue.depth(), 1, "a successful write releases one slot");
        assert_eq!(
            queue.encoded_bytes(),
            message_bytes,
            "a successful write releases exactly its encoded bytes"
        );

        client.close_outbound();
        releases.add_permits(1);
        serving.await.unwrap();
        assert_eq!(queue.depth(), 0, "close drains the remaining message");
        assert_eq!(
            queue.encoded_bytes(),
            0,
            "close releases all byte accounting"
        );
    }
}
