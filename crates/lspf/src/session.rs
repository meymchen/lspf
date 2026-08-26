//! Endpoint-neutral protocol session primitives.
//!
//! Correlation, admission, deadline constants, task ownership, writer
//! coordination, and close signaling live here so protocol endpoints share
//! one implementation. Endpoint lifecycle and registration policy remain in
//! their endpoint engines.

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
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, warn};

use crate::LspError;
use crate::client::ClientHandle;
pub(crate) use crate::client::{OutboundQueue, OutboundRegistry};
use crate::failure::{ConnectionDirection, ConnectionFailureCategory, FailureReporter};
use crate::raw::{JsonRpcError, RawMessage, RequestId};
use crate::resource_policy::ResourcePolicy;
use crate::runtime::{Runtime, TaskHandle, TaskSend};
use crate::service::{HandlerTimeout, ServiceResult};
use crate::sync::{OwnedPermit, Semaphore};
use crate::telemetry::{Completion, ConnectionTrace, Direction, Instant, Resource, ResourceAction};
use crate::transport::{TransportError, TransportReader, TransportWriter};

pub(crate) struct CloseSignal<C> {
    inner: Arc<CloseInner<C>>,
}

/// The endpoint-neutral owner of one connection's mutable protocol machinery.
///
/// Endpoint engines retain lifecycle, registration, and domain state. This
/// module owns the invariants that both directions of an LSP connection need.
pub(crate) struct ProtocolSession<R, P, C> {
    inbound: InboundRegistry,
    handler_timeout: Duration,
    tasks: TaskGroup<R>,
    out_tx: OutboundQueue,
    cancellation: CancellationToken,
    close: CloseSignal<C>,
    peer: P,
    writer_failed: fn() -> C,
    send_task: Option<TaskHandle>,
    closed: bool,
}

/// Cloneable endpoint control over shared shutdown and close mechanics.
/// Endpoint lifecycle policy remains in the endpoint that invokes it.
pub(crate) struct ProtocolControl<P: SessionPeer, C> {
    inbound: InboundRegistry,
    out_tx: OutboundQueue,
    peer: P,
    close: CloseSignal<C>,
}

impl<P: SessionPeer, C> Clone for ProtocolControl<P, C> {
    fn clone(&self) -> Self {
        Self {
            inbound: self.inbound.clone(),
            out_tx: self.out_tx.clone(),
            peer: self.peer.clone(),
            close: self.close.clone(),
        }
    }
}

impl<P: SessionPeer, C> ProtocolControl<P, C> {
    pub(crate) fn successful_shutdown(&self) {
        self.inbound.cancel_all_with_response(&self.out_tx);
        self.peer.close_pending();
    }

    pub(crate) fn request_close(&self, cause: C) {
        self.close.request(cause);
    }
}

#[derive(Clone)]
pub(crate) struct CompletionGate {
    inbound: InboundRegistry,
    outbound: OutboundQueue,
}

pub(crate) enum SessionInput {
    CloseRequested,
    OutboundFailed,
    Message(std::result::Result<RawMessage, TransportError>),
}

impl CompletionGate {
    pub(crate) fn complete(&self, reservation: Reservation, result: Result<Bytes, LspError>) {
        self.inbound.complete(&self.outbound, reservation, result);
    }
}

impl<R: Runtime, P: SessionPeer, C> ProtocolSession<R, P, C> {
    /// Create every piece of mutable protocol machinery for one connection.
    pub(crate) fn start<W, F>(
        runtime: R,
        policy: ResourcePolicy,
        writer: W,
        trace: ConnectionTrace,
        connection_span: Span,
        failure_reporter: FailureReporter,
        writer_failed: fn() -> C,
        make_peer: F,
    ) -> (Self, P)
    where
        W: TransportWriter + 'static,
        P: TaskSend + 'static,
        C: TaskSend + 'static,
        F: FnOnce(OutboundQueue, OutboundRegistry, Option<Duration>) -> P,
    {
        let (out_tx, out_rx) = OutboundQueue::bounded_with_reporter(
            policy.max_outbound_messages,
            policy.max_outbound_bytes,
            trace,
            failure_reporter.clone(),
        );
        let peer = make_peer(
            out_tx.clone(),
            OutboundRegistry::default(),
            policy.outbound_request_timeout,
        );
        let close = CloseSignal::new();
        let send_task = runtime.spawn(
            send_loop_with_trace(
                writer,
                out_rx,
                peer.clone(),
                close.clone(),
                writer_failed,
                trace,
                failure_reporter.clone(),
            )
            .instrument(connection_span),
        );
        let session = Self {
            inbound: InboundRegistry::new_with_reporter(
                policy.max_inbound_requests,
                trace,
                failure_reporter,
            ),
            handler_timeout: policy.handler_timeout,
            tasks: TaskGroup::new(runtime),
            out_tx,
            cancellation: CancellationToken::new(),
            close,
            peer: peer.clone(),
            writer_failed,
            send_task: Some(send_task),
            closed: false,
        };
        (session, peer)
    }

    /// Run the one idempotent protocol close operation.
    pub(crate) async fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.peer.close_connection();
        self.cancellation.cancel();
        self.peer.close_pending();
        self.inbound.close_all();
        self.peer.clear_endpoint_registries();
        self.tasks.abort_and_join().await;
        self.peer.close_outbound();
        if let Some(send_task) = self.send_task.take() {
            send_task.join().await;
        }
    }

    pub(crate) fn close_requested(&self) -> CancellationToken {
        self.close.requested()
    }

    pub(crate) fn control(&self) -> ProtocolControl<P, C> {
        ProtocolControl {
            inbound: self.inbound.clone(),
            out_tx: self.out_tx.clone(),
            peer: self.peer.clone(),
            close: self.close.clone(),
        }
    }

    pub(crate) fn outbound_failed(&self) -> CancellationToken {
        self.out_tx.failure()
    }

    pub(crate) fn request_close(&self, cause: C) {
        self.close.request(cause);
    }

    pub(crate) fn take_close_cause(&self) -> Option<C> {
        self.close.take_cause()
    }

    pub(crate) fn reject_inbound(&self, id: RequestId, error: LspError) {
        let _ = self.out_tx.send_required(error_response(id, &error));
    }

    pub(crate) fn complete_cancelled(&self, reservation: Reservation) {
        enqueue_encoded(
            &self.out_tx,
            reservation.id,
            Err(LspError::RequestCancelled),
        );
    }

    pub(crate) fn send_protocol_error(&self, error: JsonRpcError) {
        let _ = self
            .out_tx
            .send_required(RawMessage::ProtocolError { error });
    }

    pub(crate) fn handler_timeout(&self) -> Duration {
        self.handler_timeout
    }

    pub(crate) fn cancellation_child(&self) -> CancellationToken {
        self.cancellation.child_token()
    }

    pub(crate) fn reserve_inbound(
        &self,
        id: RequestId,
        method: &str,
        cancellable: bool,
    ) -> Result<ReservedRequest, InboundReserveError> {
        let parent = cancellable.then_some(&self.cancellation);
        self.inbound.reserve_method(id, method, parent)
    }

    pub(crate) fn complete_inbound(
        &self,
        reservation: Reservation,
        result: Result<Bytes, LspError>,
    ) {
        self.inbound.complete(&self.out_tx, reservation, result);
    }

    pub(crate) fn cancel_inbound(&self, id: &RequestId) -> Option<Reservation> {
        self.inbound.claim_cancellation(id)
    }

    pub(crate) fn cancel_all_inbound_with_response(&self) {
        self.inbound.cancel_all_with_response(&self.out_tx);
    }

    pub(crate) fn completion_gate(&self) -> CompletionGate {
        CompletionGate {
            inbound: self.inbound.clone(),
            outbound: self.out_tx.clone(),
        }
    }

    pub(crate) fn spawn<F>(&mut self, future: F, permit: Arc<OwnedPermit>)
    where
        F: Future<Output = ()> + TaskSend + 'static,
    {
        self.tasks.spawn(future, permit);
    }

    pub(crate) async fn reap_finished(&mut self) {
        self.tasks.reap_finished().await;
    }

    /// Select the next endpoint-neutral input and reap completed handler tasks
    /// immediately before returning a Transport message for dispatch.
    pub(crate) async fn next_input<Rd: TransportReader>(
        &mut self,
        reader: &mut Rd,
    ) -> SessionInput {
        let requested = self.close_requested();
        let outbound_failed = self.outbound_failed();
        let input = select_biased! {
            () = requested.cancelled().fuse() => SessionInput::CloseRequested,
            () = outbound_failed.cancelled().fuse() => SessionInput::OutboundFailed,
            message = reader.recv().fuse() => SessionInput::Message(message),
        };
        if matches!(input, SessionInput::Message(_)) {
            self.reap_finished().await;
        }
        input
    }

    /// Take the recorded close cause, applying required-writer-failure
    /// precedence after close has quiesced all connection tasks.
    pub(crate) fn final_close_cause(&self) -> C {
        let recorded = self
            .take_close_cause()
            .expect("every path out of an endpoint read-loop records its close cause");
        if self.outbound_failed().is_cancelled() {
            (self.writer_failed)()
        } else {
            recorded
        }
    }
}

impl<R, P, C> Drop for ProtocolSession<R, P, C> {
    fn drop(&mut self) {
        for task in &self.tasks.handles {
            task.handle.abort();
        }
        if let Some(send_task) = &self.send_task {
            send_task.abort();
        }
    }
}

/// Operations the shared session needs from either endpoint's peer handle.
pub(crate) trait SessionPeer: Clone {
    fn close_connection(&self);
    fn close_pending(&self);
    fn clear_endpoint_registries(&self);
    fn close_outbound(&self);
    fn outbound_closing(&self) -> CancellationToken;
    fn record_outbound_done(&self);
    fn discard_outbound(&self);
}

impl SessionPeer for ClientHandle {
    fn close_connection(&self) {
        ClientHandle::close_connection(self);
    }

    fn close_pending(&self) {
        self.outbound_registry().close_all();
    }

    fn clear_endpoint_registries(&self) {
        self.progress_registry().clear();
    }

    fn close_outbound(&self) {
        ClientHandle::close_outbound(self);
    }

    fn outbound_closing(&self) -> CancellationToken {
        ClientHandle::outbound_closing(self)
    }

    fn record_outbound_done(&self) {
        self.record_done();
    }

    fn discard_outbound(&self) {
        ClientHandle::discard_outbound(self);
    }
}

impl<C> Clone for CloseSignal<C> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct CloseInner<C> {
    cause: Mutex<Option<C>>,
    requested: CancellationToken,
}

impl<C> CloseSignal<C> {
    pub(crate) fn new() -> Self {
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
    pub(crate) fn request(&self, cause: C) {
        {
            let mut recorded = self.inner.cause.lock().unwrap();
            if recorded.is_none() {
                *recorded = Some(cause);
            }
        }
        self.inner.requested.cancel();
    }

    /// The token that fires once any caller has requested closure.
    pub(crate) fn requested(&self) -> CancellationToken {
        self.inner.requested.clone()
    }

    /// Take the recorded cause. Called once, by the read-loop, after the close
    /// operation has run.
    pub(crate) fn take_cause(&self) -> Option<C> {
        self.inner.cause.lock().unwrap().take()
    }
}

pub(crate) struct TaskGroup<R> {
    pub(crate) runtime: R,
    pub(crate) handles: Vec<InboundTask>,
}

pub(crate) struct InboundTask {
    pub(crate) handle: TaskHandle,
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
    pub(crate) fn new(runtime: R) -> Self {
        Self {
            runtime,
            handles: Vec::new(),
        }
    }

    pub(crate) fn spawn<F>(&mut self, future: F, permit: Arc<OwnedPermit>)
    where
        F: Future<Output = ()> + TaskSend + 'static,
    {
        self.handles.push(InboundTask {
            handle: self.runtime.spawn(future),
            _permit: permit,
        });
    }

    pub(crate) async fn reap_finished(&mut self) {
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

    pub(crate) async fn abort_and_join(&mut self) {
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
pub(crate) struct Reservation {
    pub(crate) id: RequestId,
    method: String,
    started: Instant,
    generation: RequestGeneration,
    pub(crate) _permit: Arc<OwnedPermit>,
}

pub(crate) struct InboundEntry {
    method: String,
    started: Instant,
    generation: RequestGeneration,
    /// `None` for `initialize`, the one request that is not cancellable.
    cancellation: Option<CancellationToken>,
    _permit: Arc<OwnedPermit>,
}

pub(crate) struct InboundInner {
    pub(crate) entries: HashMap<RequestId, InboundEntry>,
    next_generation: RequestGeneration,
}

#[derive(Clone)]
pub(crate) struct InboundRegistry {
    pub(crate) inner: Arc<Mutex<InboundInner>>,
    capacity: Arc<Semaphore>,
    trace: ConnectionTrace,
    failure_reporter: FailureReporter,
    limit: usize,
}

#[derive(Debug)]
pub(crate) enum InboundReserveError {
    DuplicateId,
    CapacityExhausted,
}

pub(crate) const INBOUND_CAPACITY_EXHAUSTED: &str = "inbound request capacity exhausted";
pub(crate) const HANDLER_DEADLINE_EXPIRED: &str = "handler deadline expired";

/// Race one admitted handler against cancellation and the deadline selected
/// by the endpoint's Layer stack.
pub(crate) async fn run_handler_with_deadline<F>(
    handler: F,
    cancellation: CancellationToken,
    handler_timeout: HandlerTimeout,
) -> ServiceResult
where
    F: Future<Output = ServiceResult>,
{
    let completion = select(Box::pin(handler), Box::pin(cancellation.cancelled()));
    let result = match select(
        Box::pin(completion),
        Box::pin(handler_timeout.wait_until_armed()),
    )
    .await
    {
        Either::Left((Either::Left((result, _)), _)) => result,
        Either::Left((Either::Right(((), handler)), _)) => cooperatively_cancelled_result(handler),
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
                    handler_timeout.finish(crate::telemetry::DeadlineAction::Expired);
                    cancellation.cancel();
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
        ServiceResult::Error(LspError::RequestCancelled) => {
            crate::telemetry::DeadlineAction::Cancelled
        }
        ServiceResult::Error(LspError::ServerError { message, .. })
            if message == HANDLER_DEADLINE_EXPIRED =>
        {
            crate::telemetry::DeadlineAction::Expired
        }
        _ => crate::telemetry::DeadlineAction::Completed,
    });
    result
}

fn cooperatively_cancelled_result<F>(handler: F) -> ServiceResult
where
    F: Future<Output = ServiceResult>,
{
    let _ = handler.now_or_never();
    ServiceResult::Error(LspError::RequestCancelled)
}

pub(crate) struct ReservedRequest {
    pub(crate) reservation: Reservation,
    pub(crate) cancellation: Option<CancellationToken>,
}

impl InboundRegistry {
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn new(capacity: usize) -> Self {
        let trace = ConnectionTrace::new();
        Self::new_with_reporter(capacity, trace, FailureReporter::new(None, trace.id()))
    }

    pub(crate) fn new_with_reporter(
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
    pub(crate) fn reserve(
        &self,
        id: RequestId,
        cancellation_parent: Option<&CancellationToken>,
    ) -> std::result::Result<ReservedRequest, InboundReserveError> {
        self.reserve_method(id, "test/request", cancellation_parent)
    }

    pub(crate) fn reserve_method(
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
            self.failure_reporter
                .report_unvalidated_inbound_method(ConnectionFailureCategory::Overload, Some(&id));
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
    pub(crate) fn complete(
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

    pub(crate) fn claim_cancellation(&self, id: &RequestId) -> Option<Reservation> {
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
    pub(crate) fn cancel_all_with_response(&self, out_tx: &OutboundQueue) {
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
    pub(crate) fn close_all(&self) {
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

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) async fn send_loop<W: TransportWriter, P: SessionPeer, C>(
    writer: W,
    out_rx: UnboundedReceiver<RawMessage>,
    peer: P,
    close: CloseSignal<C>,
    writer_failed: fn() -> C,
) {
    let trace = ConnectionTrace::new();
    send_loop_with_trace(
        writer,
        out_rx,
        peer,
        close,
        writer_failed,
        trace,
        FailureReporter::new(None, trace.id()),
    )
    .await;
}

pub(crate) async fn send_loop_with_trace<W: TransportWriter, P: SessionPeer, C>(
    mut writer: W,
    mut out_rx: UnboundedReceiver<RawMessage>,
    peer: P,
    close: CloseSignal<C>,
    writer_failed: fn() -> C,
    trace: ConnectionTrace,
    failure_reporter: FailureReporter,
) {
    let outbound_closing = peer.outbound_closing();
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
            peer.close_outbound();
            break;
        };
        // The depth counts what is still queued, so each message is decremented
        // once its transport send has succeeded or failed — including the
        // terminally failed send, after which the loop returns.
        if let Err(e) =
            send_outbound(&mut writer, msg, peer.clone(), trace, &failure_reporter).await
        {
            warn!(error = %e, "send_loop: transport write failed");
            abandon_outbound(&mut out_rx, &peer, &close, writer_failed);
            // ADR 0018: the writer reports its terminal failure and performs no
            // registry or task cleanup of its own; the engine runs the one
            // close operation. Accounting is released here because the
            // receiver is abandoning every message it retained.
            return;
        }
    }
    while let Ok(msg) = out_rx.recv().await {
        if let Err(e) =
            send_outbound(&mut writer, msg, peer.clone(), trace, &failure_reporter).await
        {
            warn!(error = %e, "send_loop: transport write failed while draining");
            abandon_outbound(&mut out_rx, &peer, &close, writer_failed);
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
        close.request(writer_failed());
    }
}

async fn send_outbound<W: TransportWriter, P: SessionPeer>(
    writer: &mut W,
    message: RawMessage,
    peer: P,
    trace: ConnectionTrace,
    failure_reporter: &FailureReporter,
) -> std::result::Result<(), TransportError> {
    trace.message(Direction::Outbound, &message);
    let method = message.method().map(str::to_owned);
    let request_id = message.id().cloned();
    let result = writer.send(message).await;
    peer.record_outbound_done();
    if result.is_err() {
        failure_reporter.report(
            ConnectionFailureCategory::Transport,
            Some(ConnectionDirection::Outbound),
            method.as_deref(),
            request_id.as_ref(),
        );
    }
    result
}

fn abandon_outbound<P: SessionPeer, C>(
    out_rx: &mut UnboundedReceiver<RawMessage>,
    peer: &P,
    close: &CloseSignal<C>,
    writer_failed: fn() -> C,
) {
    out_rx.close();
    peer.discard_outbound();
    close.request(writer_failed());
}

pub(crate) fn enqueue_encoded(
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

pub(crate) fn error_response(id: RequestId, err: &LspError) -> RawMessage {
    RawMessage::Response {
        id,
        result: Err(JsonRpcError {
            code: err.code(),
            message: err.message(),
            data: err.data().cloned(),
        }),
    }
}
