//! Endpoint-neutral protocol session.
//!
//! The session owns the inbound pipeline both endpoints share (ADR 0037):
//! reading, admission, `$/cancelRequest` claims, response correlation, handler
//! deadlines and panic backstop, and close-cause selection. It also owns the
//! bounded queues, task ownership, writer coordination, and idempotent close.
//! Endpoints pull [`SessionEvent`]s from it and keep lifecycle, registration,
//! and domain-state policy.

use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures_channel::mpsc::UnboundedReceiver;
use futures_util::FutureExt;
use futures_util::future::{Either, select};
use futures_util::select_biased;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, debug, error, warn};

use crate::LspError;
use crate::client::ClientHandle;
pub(crate) use crate::client::{OutboundQueue, OutboundRegistry};
use crate::codec::encode_body;
use crate::failure::{ConnectionDirection, ConnectionFailureCategory, FailureReporter};
use crate::raw::{JsonRpcError, RawMessage, RequestId};
use crate::resource_policy::ResourcePolicy;
use crate::runtime::{Runtime, TaskHandle, TaskSend};
use crate::service::{HandlerTimeout, ServiceResult};
use crate::sync::{OwnedPermit, Semaphore};
use crate::telemetry::{Completion, ConnectionTrace, Direction, Instant, Resource, ResourceAction};
use crate::transport::{TransportError, TransportReader, TransportWriter};

struct CloseSignal<C> {
    inner: Arc<CloseInner<C>>,
}

/// Why a connection ended, selected once the session has closed.
#[derive(Debug)]
pub(crate) enum SessionEnd<C> {
    /// The reader reached end of input.
    ReaderEof,
    /// The reader failed with a transport error.
    ReaderFailed(TransportError),
    /// The writer failed to send or shut down, or required protocol traffic
    /// could not fit within the outbound resource policy (ADR 0026).
    WriterFailed,
    /// The endpoint requested close for a reason of its own.
    Endpoint(C),
}

/// An endpoint's own close reason, named in connection telemetry.
pub(crate) trait EndpointCause {
    fn as_str(&self) -> &'static str;
}

impl<C: EndpointCause> SessionEnd<C> {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::ReaderEof => "reader_eof",
            Self::ReaderFailed(_) => "reader_failed",
            Self::WriterFailed => "writer_failed",
            Self::Endpoint(cause) => cause.as_str(),
        }
    }
}

/// What the session hands an endpoint from its read loop.
pub(crate) enum SessionEvent {
    /// A request that passed admission and must be answered exactly once.
    Request(AdmittedRequest),
    /// A notification the session does not own. `$/cancelRequest` never
    /// reaches the endpoint.
    Notification {
        method: Cow<'static, str>,
        params: Bytes,
    },
    /// Some cause requested close; the endpoint stops reading and calls
    /// [`ProtocolSession::finish`].
    Closed,
}

/// How a spawned request's completion gate settled, for endpoint registries
/// that must commit or roll back with the response.
pub(crate) enum Settled {
    /// This task's result was the response. The callback runs before the peer
    /// can observe it.
    Claimed { succeeded: bool },
    /// Cancellation or close answered the request first.
    Lost,
}

/// One inbound request holding an admission permit.
///
/// The endpoint answers it exactly once: inline with [`respond`](Self::respond),
/// or by handing it to [`ProtocolSession::spawn_request`]. Dropping it
/// unanswered sends an internal error and fails debug builds.
#[must_use = "an admitted request must be answered exactly once"]
pub(crate) struct AdmittedRequest {
    reservation: Option<Reservation>,
    params: Bytes,
    cancellation: CancellationToken,
    span: Span,
    gate: CompletionGate,
}

impl AdmittedRequest {
    pub(crate) fn id(&self) -> &RequestId {
        &self.reservation().id
    }

    pub(crate) fn method(&self) -> &str {
        &self.reservation().method
    }

    pub(crate) fn params(&self) -> &Bytes {
        &self.params
    }

    pub(crate) fn span(&self) -> &Span {
        &self.span
    }

    /// The request's cancellation token. It fires on `$/cancelRequest`, a
    /// successful `shutdown`, a handler deadline, or close; `initialize` is not
    /// cancellable by the peer, so its token fires only on close.
    pub(crate) fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Answer now. Does nothing if cancellation or close answered first.
    pub(crate) fn respond(mut self, result: Result<Bytes, LspError>) {
        let reservation = self.take_reservation();
        self.gate.complete(reservation, result);
    }

    fn reservation(&self) -> &Reservation {
        self.reservation
            .as_ref()
            .expect("an admitted request holds its reservation until answered")
    }

    fn take_reservation(&mut self) -> Reservation {
        self.reservation
            .take()
            .expect("an admitted request is answered once")
    }
}

impl Drop for AdmittedRequest {
    fn drop(&mut self) {
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        debug_assert!(
            std::thread::panicking(),
            "admitted request {:?} dropped without a response",
            reservation.id
        );
        self.gate.complete(
            reservation,
            Err(LspError::internal("request dropped without a response")),
        );
    }
}

/// The endpoint-neutral owner of one connection's mutable protocol machinery.
pub(crate) struct ProtocolSession<R, P, C> {
    inbound: InboundRegistry,
    handler_timeout: Duration,
    tasks: TaskGroup<R>,
    out_tx: OutboundQueue,
    cancellation: CancellationToken,
    close: CloseSignal<SessionEnd<C>>,
    peer: P,
    trace: ConnectionTrace,
    failure_reporter: FailureReporter,
    send_task: Option<TaskHandle>,
    closed: bool,
}

/// Cloneable endpoint control over shared shutdown and close mechanics.
/// Endpoint lifecycle policy remains in the endpoint that invokes it.
pub(crate) struct ProtocolControl<P: SessionPeer, C> {
    inbound: InboundRegistry,
    out_tx: OutboundQueue,
    peer: P,
    close: CloseSignal<SessionEnd<C>>,
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
        self.close.request(SessionEnd::Endpoint(cause));
    }
}

#[derive(Clone)]
struct CompletionGate {
    inbound: InboundRegistry,
    outbound: OutboundQueue,
}

enum SessionInput {
    CloseRequested,
    OutboundFailed,
    Message(Result<RawMessage, TransportError>),
}

impl CompletionGate {
    fn complete(&self, reservation: Reservation, result: Result<Bytes, LspError>) {
        self.inbound.complete(&self.outbound, reservation, result);
    }

    /// Claim one request completion, run endpoint-specific registry mutation,
    /// then enqueue the response. The callback runs only for the winning
    /// completion path and before the peer can observe its response.
    fn try_complete_with<F>(
        &self,
        reservation: Reservation,
        result: Result<Bytes, LspError>,
        on_claim: F,
    ) -> bool
    where
        F: FnOnce(),
    {
        self.inbound
            .try_complete_with(&self.outbound, reservation, result, on_claim)
    }
}

impl<R: Runtime, P: SessionPeer, C: EndpointCause> ProtocolSession<R, P, C> {
    /// Create every piece of mutable protocol machinery for one connection.
    pub(crate) fn start<W, F>(
        runtime: R,
        policy: ResourcePolicy,
        writer: W,
        trace: ConnectionTrace,
        connection_span: Span,
        failure_reporter: FailureReporter,
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
                trace,
                failure_reporter.clone(),
            )
            .instrument(connection_span),
        );
        let session = Self {
            inbound: InboundRegistry::new_with_reporter(
                policy.max_inbound_requests,
                trace,
                failure_reporter.clone(),
            ),
            handler_timeout: policy.handler_timeout,
            tasks: TaskGroup::new(runtime),
            out_tx,
            cancellation: CancellationToken::new(),
            close,
            peer: peer.clone(),
            trace,
            failure_reporter,
            send_task: Some(send_task),
            closed: false,
        };
        (session, peer)
    }

    pub(crate) fn control(&self) -> ProtocolControl<P, C> {
        ProtocolControl {
            inbound: self.inbound.clone(),
            out_tx: self.out_tx.clone(),
            peer: self.peer.clone(),
            close: self.close.clone(),
        }
    }

    /// Request close for an endpoint reason. The first requested cause wins;
    /// the next [`next_event`](Self::next_event) returns [`SessionEvent::Closed`].
    pub(crate) fn request_close(&self, cause: C) {
        self.close.request(SessionEnd::Endpoint(cause));
    }

    /// Cancel and answer every admitted request, as a successful `shutdown`
    /// requires.
    pub(crate) fn cancel_all_inbound_with_response(&self) {
        self.inbound.cancel_all_with_response(&self.out_tx);
    }

    /// Read until there is something for the endpoint.
    ///
    /// Responses, `$/cancelRequest`, inbound protocol errors, and requests that
    /// fail admission are handled here and never surface. A reader or writer
    /// failure records its cause and returns [`SessionEvent::Closed`].
    /// Cancel-safe: dropping the future loses no processed message.
    pub(crate) async fn next_event<Rd: TransportReader>(
        &mut self,
        reader: &mut Rd,
    ) -> SessionEvent {
        loop {
            let message = match self.next_input(reader).await {
                SessionInput::CloseRequested => return SessionEvent::Closed,
                SessionInput::OutboundFailed => {
                    self.close.request(SessionEnd::WriterFailed);
                    return SessionEvent::Closed;
                }
                SessionInput::Message(Ok(message)) => message,
                SessionInput::Message(Err(TransportError::Closed)) => {
                    warn!("transport closed by peer before the connection ended");
                    self.close.request(SessionEnd::ReaderEof);
                    return SessionEvent::Closed;
                }
                SessionInput::Message(Err(error)) => {
                    let category = match &error {
                        TransportError::Malformed(_) | TransportError::OversizedMessage { .. } => {
                            ConnectionFailureCategory::Framing
                        }
                        TransportError::Io(_)
                        | TransportError::Serde(_)
                        | TransportError::Closed => ConnectionFailureCategory::Transport,
                    };
                    self.failure_reporter.report(
                        category,
                        Some(ConnectionDirection::Inbound),
                        None,
                        None,
                    );
                    self.close.request(SessionEnd::ReaderFailed(error));
                    return SessionEvent::Closed;
                }
            };
            self.trace.message(Direction::Inbound, &message);
            match message {
                RawMessage::Request { id, method, params } => {
                    if let Some(request) = self.admit(id, method, params) {
                        return SessionEvent::Request(request);
                    }
                }
                RawMessage::Notification { method, params } if method == "$/cancelRequest" => {
                    self.claim_cancellation(&params);
                }
                RawMessage::Notification { method, params } => {
                    return SessionEvent::Notification { method, params };
                }
                RawMessage::Response { id, result } => self.correlate(id, result),
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
        }
    }

    /// Run `handler` as a connection-owned task and answer with its result.
    ///
    /// The handler receives the request's [`HandlerTimeout`] and decides when to
    /// arm it. The session races it against cancellation and that deadline,
    /// isolates a panic while constructing or polling it, and answers exactly
    /// once through the completion gate.
    pub(crate) fn spawn_request<H, Fut>(&mut self, request: AdmittedRequest, handler: H)
    where
        H: FnOnce(HandlerTimeout) -> Fut + TaskSend + 'static,
        Fut: Future<Output = ServiceResult> + TaskSend + 'static,
    {
        self.spawn_admitted(request, handler, None::<fn(Settled)>);
    }

    /// [`spawn_request`](Self::spawn_request), reporting how the completion
    /// gate settled so an endpoint registry can commit or roll back with it.
    pub(crate) fn spawn_request_with_claim<H, Fut, S>(
        &mut self,
        request: AdmittedRequest,
        handler: H,
        on_settled: S,
    ) where
        H: FnOnce(HandlerTimeout) -> Fut + TaskSend + 'static,
        Fut: Future<Output = ServiceResult> + TaskSend + 'static,
        S: FnOnce(Settled) + TaskSend + 'static,
    {
        self.spawn_admitted(request, handler, Some(on_settled));
    }

    /// Spawn connection-owned notification work for a registered `method`
    /// without charging the inbound request budget. A panic is isolated and
    /// reported; close aborts and joins the task.
    pub(crate) fn spawn_notification<F>(&mut self, method: &'static str, future: F)
    where
        F: Future<Output = ()> + TaskSend + 'static,
    {
        let failure_reporter = self.failure_reporter.clone();
        self.tasks.spawn_notification(async move {
            if AssertUnwindSafe(future).catch_unwind().await.is_err() {
                failure_reporter.report(
                    ConnectionFailureCategory::PanicIsolation,
                    Some(ConnectionDirection::Inbound),
                    Some(method),
                    None,
                );
                error!(%method, "panic isolated while dispatching notification");
            }
        });
    }

    /// Run the one idempotent close operation and select the connection's
    /// ending: a required-writer failure overrides the recorded cause because
    /// close has now quiesced every task that could fail one (ADR 0026).
    pub(crate) async fn finish(&mut self) -> SessionEnd<C> {
        self.close().await;
        let recorded = self
            .close
            .take_cause()
            .expect("every path out of an endpoint read loop records its close cause");
        let end = if self.out_tx.failure().is_cancelled() {
            SessionEnd::WriterFailed
        } else {
            recorded
        };
        self.trace.connection_closed(end.as_str());
        end
    }

    async fn close(&mut self) {
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

    /// Select the next input and reap completed handler tasks immediately
    /// before returning a Transport message.
    async fn next_input<Rd: TransportReader>(&mut self, reader: &mut Rd) -> SessionInput {
        let requested = self.close.requested();
        let outbound_failed = self.out_tx.failure();
        let input = select_biased! {
            () = requested.cancelled().fuse() => SessionInput::CloseRequested,
            () = outbound_failed.cancelled().fuse() => SessionInput::OutboundFailed,
            message = reader.recv().fuse() => SessionInput::Message(message),
        };
        if matches!(input, SessionInput::Message(_)) {
            self.tasks.reap_finished().await;
        }
        input
    }

    /// Admission happens before request-scoped cancellation state, parameter
    /// decoding, and task creation. A rejected request is answered here.
    fn admit(
        &self,
        id: RequestId,
        method: Cow<'static, str>,
        params: Bytes,
    ) -> Option<AdmittedRequest> {
        let span = self.trace.request_span(&method, &id);
        let cancellable = method != "initialize";
        match self.inbound.reserve_method(
            id.clone(),
            &method,
            cancellable.then_some(&self.cancellation),
        ) {
            Ok(reserved) => Some(AdmittedRequest {
                reservation: Some(reserved.reservation),
                params,
                cancellation: reserved
                    .cancellation
                    .unwrap_or_else(|| self.cancellation.child_token()),
                span,
                gate: self.completion_gate(),
            }),
            Err(rejection) => {
                let error = match rejection {
                    InboundReserveError::DuplicateId => {
                        self.failure_reporter.report_unvalidated_inbound_method(
                            ConnectionFailureCategory::Protocol,
                            Some(&id),
                        );
                        LspError::invalid_request("duplicate request id")
                    }
                    InboundReserveError::CapacityExhausted => {
                        LspError::ServerCancelled(INBOUND_CAPACITY_EXHAUSTED.to_string())
                    }
                };
                self.trace.request_completed(
                    &method,
                    &id,
                    Instant::now(),
                    Direction::Inbound,
                    Completion::Rejected,
                );
                let _ = self.out_tx.send_required(error_response(id, &error));
                None
            }
        }
    }

    fn claim_cancellation(&self, params: &Bytes) {
        #[derive(serde::Deserialize)]
        struct CancelParams {
            id: RequestId,
        }
        let bytes: &[u8] = if params.is_empty() { b"{}" } else { params };
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
                self.failure_reporter.report(
                    ConnectionFailureCategory::Protocol,
                    Some(ConnectionDirection::Inbound),
                    Some("$/cancelRequest"),
                    None,
                );
                debug!(%error, "ignoring malformed $/cancelRequest");
            }
        }
    }

    /// Only positive numeric IDs are allocated by `OutboundRegistry`.
    fn correlate(&self, id: RequestId, result: Result<Bytes, JsonRpcError>) {
        let delivered = match &id {
            RequestId::Number(number) if *number > 0 => {
                self.peer.complete_outbound(*number as u32, result)
            }
            _ => false,
        };
        if !delivered {
            self.failure_reporter.report(
                ConnectionFailureCategory::Protocol,
                Some(ConnectionDirection::Inbound),
                None,
                Some(&id),
            );
            debug!(?id, "ignoring response with unknown or non-numeric id");
        }
    }

    fn completion_gate(&self) -> CompletionGate {
        CompletionGate {
            inbound: self.inbound.clone(),
            outbound: self.out_tx.clone(),
        }
    }

    fn spawn_admitted<H, Fut, S>(
        &mut self,
        mut request: AdmittedRequest,
        handler: H,
        on_settled: Option<S>,
    ) where
        H: FnOnce(HandlerTimeout) -> Fut + TaskSend + 'static,
        Fut: Future<Output = ServiceResult> + TaskSend + 'static,
        S: FnOnce(Settled) + TaskSend + 'static,
    {
        let reservation = request.take_reservation();
        let cancellation = request.cancellation.clone();
        let span = request.span.clone();
        let gate = request.gate.clone();
        drop(request);
        let permit = Arc::clone(&reservation._permit);
        let handler_timeout = HandlerTimeout::new(
            self.handler_timeout,
            self.trace,
            reservation.method.clone(),
            reservation.id.clone(),
        );
        let failure_reporter = self.failure_reporter.clone();
        let id = reservation.id.clone();
        self.tasks.spawn(
            async move {
                let timeout = handler_timeout.clone();
                // Catch both constructing and polling the handler so a panic
                // follows the normal completion path, including admission
                // release and any endpoint registry rollback.
                let isolated = AssertUnwindSafe(async move { handler(timeout).await })
                    .catch_unwind()
                    .map(move |result| {
                        result.unwrap_or_else(|_| {
                            failure_reporter.report_unvalidated_inbound_method(
                                ConnectionFailureCategory::PanicIsolation,
                                Some(&id),
                            );
                            error!("panic isolated while dispatching request");
                            ServiceResult::Error(LspError::internal("user dispatch panicked"))
                        })
                    });
                let result = match run_handler_with_deadline(
                    isolated,
                    cancellation,
                    handler_timeout,
                )
                .await
                {
                    ServiceResult::Response(value) => encode_body(&value),
                    ServiceResult::Error(error) => Err(error),
                    ServiceResult::NoResponse => {
                        Err(LspError::internal("request service returned no response"))
                    }
                };
                match on_settled {
                    None => gate.complete(reservation, result),
                    Some(on_settled) => {
                        let succeeded = result.is_ok();
                        let mut on_settled = Some(on_settled);
                        let claimed = gate.try_complete_with(reservation, result, || {
                            if let Some(on_settled) = on_settled.take() {
                                on_settled(Settled::Claimed { succeeded });
                            }
                        });
                        if !claimed && let Some(on_settled) = on_settled.take() {
                            on_settled(Settled::Lost);
                        }
                    }
                }
            }
            .instrument(span),
            permit,
        );
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
    fn complete_outbound(&self, id: u32, result: Result<Bytes, JsonRpcError>) -> bool;
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

    fn complete_outbound(&self, id: u32, result: Result<Bytes, JsonRpcError>) -> bool {
        self.outbound_registry().complete(id, result)
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
    fn request(&self, cause: C) {
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
    fn take_cause(&self) -> Option<C> {
        self.inner.cause.lock().unwrap().take()
    }
}

struct TaskGroup<R> {
    runtime: R,
    handles: Vec<ConnectionTask>,
}

struct ConnectionTask {
    handle: TaskHandle,
    _permit: Option<Arc<OwnedPermit>>,
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
        self.handles.push(ConnectionTask {
            handle: self.runtime.spawn(future),
            _permit: Some(permit),
        });
    }

    fn spawn_notification<F>(&mut self, future: F)
    where
        F: Future<Output = ()> + TaskSend + 'static,
    {
        self.handles.push(ConnectionTask {
            handle: self.runtime.spawn(future),
            _permit: None,
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

/// Race one admitted handler against cancellation and the deadline selected
/// by the endpoint's Layer stack.
async fn run_handler_with_deadline<F>(
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
                    ServiceResult::Error(LspError::ServerCancelled(
                        HANDLER_DEADLINE_EXPIRED.to_string(),
                    ))
                }
            }
        }
    };
    handler_timeout.finish(match &result {
        ServiceResult::Error(LspError::RequestCancelled) => {
            crate::telemetry::DeadlineAction::Cancelled
        }
        ServiceResult::Error(LspError::ServerCancelled(message))
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
    fn complete(
        &self,
        out_tx: &OutboundQueue,
        reservation: Reservation,
        result: std::result::Result<Bytes, LspError>,
    ) {
        self.try_complete_with(out_tx, reservation, result, || {});
    }

    fn try_complete_with<F>(
        &self,
        out_tx: &OutboundQueue,
        reservation: Reservation,
        result: std::result::Result<Bytes, LspError>,
        on_claim: F,
    ) -> bool
    where
        F: FnOnce(),
    {
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
            let callback = std::panic::catch_unwind(std::panic::AssertUnwindSafe(on_claim));
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
            if let Err(payload) = callback {
                std::panic::resume_unwind(payload);
            }
            true
        } else {
            false
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

#[cfg(all(test, not(target_arch = "wasm32")))]
async fn send_loop<W: TransportWriter, P: SessionPeer, C>(
    writer: W,
    out_rx: UnboundedReceiver<RawMessage>,
    peer: P,
    close: CloseSignal<SessionEnd<C>>,
) {
    let trace = ConnectionTrace::new();
    send_loop_with_trace(
        writer,
        out_rx,
        peer,
        close,
        trace,
        FailureReporter::new(None, trace.id()),
    )
    .await;
}

async fn send_loop_with_trace<W: TransportWriter, P: SessionPeer, C>(
    mut writer: W,
    mut out_rx: UnboundedReceiver<RawMessage>,
    peer: P,
    close: CloseSignal<SessionEnd<C>>,
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
            abandon_outbound(&mut out_rx, &peer, &close);
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
            abandon_outbound(&mut out_rx, &peer, &close);
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
        close.request(SessionEnd::WriterFailed);
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
    close: &CloseSignal<SessionEnd<C>>,
) {
    out_rx.close();
    peer.discard_outbound();
    close.request(SessionEnd::WriterFailed);
}

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
        Err(LspError::ServerCancelled(message)) if message == HANDLER_DEADLINE_EXPIRED => {
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::borrow::Cow;

    use tokio::sync::mpsc;

    use crate::runtime::default_runtime;

    use super::*;

    #[derive(Debug, PartialEq)]
    enum TestCause {
        Exit,
        Disconnect,
    }

    impl EndpointCause for TestCause {
        fn as_str(&self) -> &'static str {
            match self {
                Self::Exit => "exit",
                Self::Disconnect => "disconnect",
            }
        }
    }

    struct ChannelReader(mpsc::UnboundedReceiver<Result<RawMessage, TransportError>>);

    impl TransportReader for ChannelReader {
        async fn recv(&mut self) -> Result<RawMessage, TransportError> {
            self.0.recv().await.unwrap_or(Err(TransportError::Closed))
        }
    }

    /// Records every sent message. Each send first takes one permit from
    /// `gate`, so a test can hold a write in flight.
    struct GatedWriter {
        sent: mpsc::UnboundedSender<RawMessage>,
        started: Arc<tokio::sync::Notify>,
        gate: Arc<tokio::sync::Semaphore>,
    }

    impl TransportWriter for GatedWriter {
        async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
            self.started.notify_one();
            self.gate
                .acquire()
                .await
                .expect("the test keeps the gate open")
                .forget();
            self.sent.send(message).map_err(|_| TransportError::Closed)
        }

        async fn shutdown(self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    /// One session over in-memory transport halves, driven through the same
    /// interface the endpoints use.
    struct Harness {
        session: ProtocolSession<crate::runtime::TokioRuntime, ClientHandle, TestCause>,
        reader: ChannelReader,
        input: Option<mpsc::UnboundedSender<Result<RawMessage, TransportError>>>,
        output: mpsc::UnboundedReceiver<RawMessage>,
        started: Arc<tokio::sync::Notify>,
        gate: Arc<tokio::sync::Semaphore>,
    }

    impl Harness {
        fn start(policy: ResourcePolicy) -> Self {
            let (input, incoming) = mpsc::unbounded_channel();
            let (sent, output) = mpsc::unbounded_channel();
            let started = Arc::new(tokio::sync::Notify::new());
            let gate = Arc::new(tokio::sync::Semaphore::new(
                tokio::sync::Semaphore::MAX_PERMITS,
            ));
            let trace = ConnectionTrace::new();
            let (session, _peer) = ProtocolSession::start(
                default_runtime(),
                policy,
                GatedWriter {
                    sent,
                    started: Arc::clone(&started),
                    gate: Arc::clone(&gate),
                },
                trace,
                Span::none(),
                FailureReporter::new(None, trace.id()),
                ClientHandle::new,
            );
            Self {
                session,
                reader: ChannelReader(incoming),
                input: Some(input),
                output,
                started,
                gate,
            }
        }

        fn send(&self, message: RawMessage) {
            self.input
                .as_ref()
                .expect("input is still open")
                .send(Ok(message))
                .unwrap();
        }

        fn fail_reader(&self, error: TransportError) {
            self.input
                .as_ref()
                .expect("input is still open")
                .send(Err(error))
                .unwrap();
        }

        fn end_input(&mut self) {
            self.input = None;
        }

        async fn next_event(&mut self) -> SessionEvent {
            tokio::time::timeout(
                Duration::from_secs(2),
                self.session.next_event(&mut self.reader),
            )
            .await
            .expect("an event within the watchdog")
        }

        async fn next_request(&mut self) -> AdmittedRequest {
            match self.next_event().await {
                SessionEvent::Request(request) => request,
                SessionEvent::Notification { method, .. } => {
                    panic!("expected a request, got notification {method}")
                }
                SessionEvent::Closed => panic!("expected a request, got close"),
            }
        }

        async fn sent(&mut self) -> RawMessage {
            tokio::time::timeout(Duration::from_secs(2), self.output.recv())
                .await
                .expect("a sent message within the watchdog")
                .expect("the writer is still open")
        }
    }

    fn request(id: i32, method: &'static str) -> RawMessage {
        RawMessage::Request {
            id: RequestId::Number(id),
            method: Cow::Borrowed(method),
            params: Bytes::from_static(b"null"),
        }
    }

    fn error_of(message: RawMessage) -> (RequestId, i32, String) {
        match message {
            RawMessage::Response {
                id,
                result: Err(error),
            } => (id, error.code, error.message),
            other => panic!("expected an error response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn admission_rejections_are_answered_without_reaching_the_endpoint() {
        let mut harness = Harness::start(ResourcePolicy {
            max_inbound_requests: 1,
            ..ResourcePolicy::default()
        });
        harness.send(request(1, "test/admitted"));
        harness.send(request(1, "test/duplicate"));
        harness.send(request(2, "test/over-capacity"));
        harness.end_input();

        let admitted = harness.next_request().await;
        assert_eq!(admitted.id(), &RequestId::Number(1));
        assert!(matches!(harness.next_event().await, SessionEvent::Closed));

        let (id, code, _) = error_of(harness.sent().await);
        assert_eq!((id, code), (RequestId::Number(1), -32600));
        let (id, code, message) = error_of(harness.sent().await);
        assert_eq!((id, code), (RequestId::Number(2), -32802));
        assert_eq!(message, INBOUND_CAPACITY_EXHAUSTED);

        admitted.respond(encode_body(&"done"));
        assert!(matches!(
            harness.sent().await,
            RawMessage::Response { result: Ok(_), .. }
        ));
        assert!(matches!(
            harness.session.finish().await,
            SessionEnd::ReaderEof
        ));
    }

    #[tokio::test]
    async fn cancel_request_answers_a_spawned_request_exactly_once() {
        let mut harness = Harness::start(ResourcePolicy::default());
        harness.send(request(1, "test/slow"));
        let slow = harness.next_request().await;
        harness
            .session
            .spawn_request(slow, |_timeout| std::future::pending());

        harness.send(RawMessage::Notification {
            method: Cow::Borrowed("$/cancelRequest"),
            params: Bytes::from_static(br#"{"id":1}"#),
        });
        harness.send(request(2, "test/next"));
        let next = harness.next_request().await;
        assert_eq!(
            next.id(),
            &RequestId::Number(2),
            "cancellation never surfaces"
        );
        let (id, code, _) = error_of(harness.sent().await);
        assert_eq!((id, code), (RequestId::Number(1), -32800));

        next.respond(encode_body(&"done"));
        harness.end_input();
        assert!(matches!(harness.next_event().await, SessionEvent::Closed));
        harness.session.finish().await;
        assert!(matches!(
            harness.output.recv().await,
            Some(RawMessage::Response {
                id: RequestId::Number(2),
                result: Ok(_)
            })
        ));
        assert!(
            harness.output.recv().await.is_none(),
            "the cancelled task's late result was never sent"
        );
    }

    #[tokio::test]
    async fn a_panicking_handler_is_answered_and_releases_its_admission() {
        let mut harness = Harness::start(ResourcePolicy {
            max_inbound_requests: 1,
            ..ResourcePolicy::default()
        });
        for attempt in ["construct", "poll"] {
            harness.send(request(1, "test/panics"));
            let request = harness.next_request().await;
            if attempt == "construct" {
                harness
                    .session
                    .spawn_request(request, |_timeout| -> std::future::Ready<ServiceResult> {
                        panic!("construct panic")
                    });
            } else {
                harness.session.spawn_request(request, |_timeout| async {
                    tokio::task::yield_now().await;
                    panic!("poll panic")
                });
            }
            let (id, code, message) = error_of(harness.sent().await);
            assert_eq!((id, code), (RequestId::Number(1), -32603));
            assert_eq!(message, "user dispatch panicked");
        }

        // The one admission slot and the reused ID are both free again.
        harness.send(request(1, "test/healthy"));
        harness.next_request().await.respond(encode_body(&"ok"));
        harness.end_input();
        assert!(matches!(harness.next_event().await, SessionEvent::Closed));
        harness.session.finish().await;
    }

    #[tokio::test]
    async fn a_panicking_notification_task_is_isolated() {
        let mut harness = Harness::start(ResourcePolicy::default());
        harness
            .session
            .spawn_notification("test/panics", async { panic!("notification panic") });
        harness.send(request(1, "test/after"));
        harness.next_request().await.respond(encode_body(&"ok"));
        assert!(matches!(
            harness.sent().await,
            RawMessage::Response { result: Ok(_), .. }
        ));
        harness.end_input();
        assert!(matches!(harness.next_event().await, SessionEvent::Closed));
        assert!(matches!(
            harness.session.finish().await,
            SessionEnd::ReaderEof
        ));
    }

    #[tokio::test]
    async fn the_first_requested_cause_is_the_ending() {
        let mut harness = Harness::start(ResourcePolicy::default());
        harness.session.request_close(TestCause::Exit);
        harness
            .session
            .control()
            .request_close(TestCause::Disconnect);
        harness.end_input();

        assert!(matches!(harness.next_event().await, SessionEvent::Closed));
        assert!(matches!(
            harness.session.finish().await,
            SessionEnd::Endpoint(TestCause::Exit)
        ));
    }

    #[tokio::test]
    async fn a_reader_failure_ends_the_connection_with_its_error() {
        let mut harness = Harness::start(ResourcePolicy::default());
        harness.fail_reader(TransportError::Malformed("bad".into()));

        assert!(matches!(harness.next_event().await, SessionEvent::Closed));
        assert!(matches!(
            harness.session.finish().await,
            SessionEnd::ReaderFailed(TransportError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn a_late_required_send_failure_overrides_an_earlier_cause() {
        let mut harness = Harness::start(ResourcePolicy {
            max_outbound_messages: 1,
            ..ResourcePolicy::default()
        });
        harness
            .gate
            .forget_permits(tokio::sync::Semaphore::MAX_PERMITS);
        harness.send(request(1, "test/first"));
        harness.send(request(2, "test/second"));
        let first = harness.next_request().await;
        let second = harness.next_request().await;

        // The first response holds the only outbound slot while its write is
        // in flight, so the second required response cannot be admitted.
        first.respond(encode_body(&"first"));
        harness.started.notified().await;
        harness.session.request_close(TestCause::Exit);
        second.respond(encode_body(&"second"));

        harness
            .gate
            .add_permits(tokio::sync::Semaphore::MAX_PERMITS);
        assert!(matches!(
            harness.session.finish().await,
            SessionEnd::WriterFailed
        ));
    }

    #[tokio::test]
    #[should_panic(expected = "dropped without a response")]
    async fn an_unanswered_admitted_request_fails_debug_builds() {
        let mut harness = Harness::start(ResourcePolicy::default());
        harness.send(request(1, "test/forgotten"));
        drop(harness.next_request().await);
    }

    /// A peer may reuse a request ID once the previous request under it has
    /// been answered. The completion gate is scoped to the reservation, not the
    /// ID, so the first request's task cannot answer the second request when it
    /// finishes after its own entry was claimed.
    #[test]
    fn a_stale_reservation_cannot_claim_a_reused_request_id() {
        let (out_tx, mut out_rx) =
            OutboundQueue::new(crate::ResourcePolicy::default().max_outbound_messages);
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
        let (out_tx, _out_rx) =
            OutboundQueue::new(crate::ResourcePolicy::default().max_outbound_messages);
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

    // --- Outbound queue depth observability tests ----------------------------

    enum TestOutboundNotification {}

    impl crate::types::notification::Notification for TestOutboundNotification {
        type Params = serde_json::Value;
        const METHOD: &'static str = "test/outbound-notification";
    }

    enum TestOutboundRequest {}

    impl crate::types::request::Request for TestOutboundRequest {
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
        let (queue, _rx) =
            OutboundQueue::new(crate::ResourcePolicy::default().max_outbound_messages);
        let client = ClientHandle::new(queue.clone(), OutboundRegistry::default(), None);

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
        let client = ClientHandle::new(queue.clone(), OutboundRegistry::default(), None);
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
        let close: CloseSignal<SessionEnd<TestCause>> = CloseSignal::new();
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
            matches!(close.take_cause(), Some(SessionEnd::WriterFailed)),
            "the writer reports its terminal failure"
        );
    }

    #[tokio::test]
    async fn draining_after_close_decrements_every_message_in_order() {
        let (queue, rx) = OutboundQueue::new(2);
        let client = ClientHandle::new(queue.clone(), OutboundRegistry::default(), None);
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
        let close: CloseSignal<SessionEnd<TestCause>> = CloseSignal::new();
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
        let client = ClientHandle::new(queue.clone(), OutboundRegistry::default(), None);
        queue.send(send_loop_message(0)).unwrap();
        queue.send(send_loop_message(1)).unwrap();

        let started = Arc::new(tokio::sync::Notify::new());
        let releases = Arc::new(tokio::sync::Semaphore::new(0));
        let writer = SlowWriter {
            started: started.clone(),
            releases: releases.clone(),
        };
        let close: CloseSignal<SessionEnd<TestCause>> = CloseSignal::new();
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
