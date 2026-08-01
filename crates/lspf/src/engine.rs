//! Connection-owned protocol engine for the 0.2 `Server<S>`.
//!
//! This slice serves a connection end to end for the lifecycle plus typed
//! custom requests, notifications, and commands. `initialize` is the one
//! bounded transaction that can conditionally extend the Router, freeze it,
//! generate capabilities, establish the connection's [`Workspace`],
//! [`Documents`], and negotiated position encoding, and run the
//! `on_initialize` lifecycle hook — all without exposing partial state
//! (ADR 0017, ADR 0018). Inbound requests reserve their IDs before user work
//! is spawned; the engine's atomic completion gate then arbitrates success,
//! errors, and cancellation.
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

use bytes::Bytes;
use futures_util::future::{Either, select};
use lsp_types::{InitializeParams, InitializeResult};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, debug, info_span, warn};

use crate::builder::{
    ConfigureInitialize, InitializeRegistrar, OnInitialize, Registrations, Server,
};
use crate::client::{Client, OutboundRegistry};
use crate::codec::{decode_params, decode_value, encode_body};
use crate::context::Context;
use crate::documents::Documents;
use crate::error::Error;
use crate::raw::{JsonRpcError, RawMessage, RequestId};
use crate::runtime::{Runtime, TaskHandle, TaskSend, default_runtime};
use crate::service::{IncomingCall, ServiceResult, UserLayer, UserService, build_service_stack};
use crate::transport::{Transport, TransportError, TransportReader, TransportWriter};
use crate::workspace::Workspace;
use crate::{LspError, Result};

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
    Exit { code: i32 },
    /// The peer closed the transport before sending `exit`.
    TransportClosed,
    /// The writer half failed terminally, so no further response could reach
    /// the peer.
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
    /// The writer failed to send or to shut down.
    WriterFailed,
    /// A failed initialize transaction terminated the connection (ADR 0018).
    InitializeFailed,
}

impl CloseCause {
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

    /// Request the one close operation. The first caller records `cause` and
    /// wakes the read-loop; a later caller leaves the recorded cause untouched
    /// and observes that same close rather than starting a second one.
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
    let (reader, writer) = transport.split();
    let (out_tx, out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let client = Client::new(out_tx.clone(), OutboundRegistry::default());
    let close = CloseSignal::new();
    let runtime = default_runtime();
    let send_task = runtime.spawn(send_loop(writer, out_rx, client.clone(), close.clone()));
    ProtocolEngine::new(server, runtime, out_tx, client, close, send_task)
        .serve(reader)
        .await
}

struct TaskGroup<R> {
    runtime: R,
    handles: Vec<TaskHandle>,
}

impl<R: Runtime> TaskGroup<R> {
    fn new(runtime: R) -> Self {
        Self {
            runtime,
            handles: Vec::new(),
        }
    }

    fn spawn<F>(&mut self, future: F)
    where
        F: Future<Output = ()> + TaskSend + 'static,
    {
        self.handles.push(self.runtime.spawn(future));
    }

    async fn reap_finished(&mut self) {
        let mut running = Vec::with_capacity(self.handles.len());
        for handle in std::mem::take(&mut self.handles) {
            if handle.is_finished() {
                handle.join().await;
            } else {
                running.push(handle);
            }
        }
        self.handles = running;
    }

    async fn abort_and_join(&mut self) {
        for handle in &self.handles {
            handle.abort();
        }
        self.join_all().await;
    }

    async fn join_all(&mut self) {
        for handle in std::mem::take(&mut self.handles) {
            handle.join().await;
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
#[derive(Clone)]
struct Reservation {
    id: RequestId,
    generation: u64,
}

struct InboundEntry {
    generation: u64,
    /// `None` for `initialize`, the one request that is not cancellable.
    cancellation: Option<CancellationToken>,
}

#[derive(Default)]
struct InboundInner {
    entries: HashMap<RequestId, InboundEntry>,
    next_generation: u64,
}

#[derive(Clone, Default)]
struct InboundRegistry {
    inner: Arc<Mutex<InboundInner>>,
}

impl InboundRegistry {
    /// Reserve `id` for a new request, or return `None` if it is already in
    /// flight — a duplicate never replaces or cancels the original (ADR 0018).
    fn reserve(
        &self,
        id: RequestId,
        cancellation: Option<CancellationToken>,
    ) -> Option<Reservation> {
        let mut inner = self.inner.lock().unwrap();
        if inner.entries.contains_key(&id) {
            return None;
        }
        let generation = inner.next_generation;
        inner.next_generation += 1;
        inner.entries.insert(
            id.clone(),
            InboundEntry {
                generation,
                cancellation,
            },
        );
        Some(Reservation { id, generation })
    }

    /// Claim the completion gate for `reservation` and enqueue its one response.
    /// Does nothing if some other path already claimed that entry.
    fn complete(
        &self,
        out_tx: &UnboundedSender<RawMessage>,
        reservation: Reservation,
        result: std::result::Result<Bytes, LspError>,
    ) {
        let claimed = {
            let mut inner = self.inner.lock().unwrap();
            match inner.entries.get(&reservation.id) {
                Some(entry) if entry.generation == reservation.generation => {
                    inner.entries.remove(&reservation.id).is_some()
                }
                _ => false,
            }
        };
        if claimed {
            enqueue_encoded(out_tx, reservation.id, result);
        }
    }

    fn complete_cancellation(&self, out_tx: &UnboundedSender<RawMessage>, id: &RequestId) {
        let token = {
            let mut inner = self.inner.lock().unwrap();
            match inner.entries.get(id) {
                Some(entry) if entry.cancellation.is_some() => inner
                    .entries
                    .remove(id)
                    .and_then(|entry| entry.cancellation),
                _ => None,
            }
        };
        if let Some(token) = token {
            token.cancel();
            enqueue_encoded(out_tx, id.clone(), Err(LspError::RequestCancelled));
        }
    }

    /// Cancel and answer every still-registered request, emptying the registry.
    ///
    /// Used by a successful `shutdown`, which leaves the connection alive long
    /// enough to deliver each cancellation. Removing the entry also claims the
    /// completion gate, so the handler's own late result is dropped and every
    /// cancelled request still receives exactly one response.
    fn cancel_all_with_response(&self, out_tx: &UnboundedSender<RawMessage>) {
        let entries = std::mem::take(&mut self.inner.lock().unwrap().entries);
        for (id, entry) in entries {
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
        for cancellation in entries.into_values().filter_map(|entry| entry.cancellation) {
            cancellation.cancel();
        }
    }
}

#[derive(serde::Deserialize)]
struct CancelParams {
    id: RequestId,
}

async fn send_loop<W: TransportWriter>(
    mut writer: W,
    mut out_rx: UnboundedReceiver<RawMessage>,
    client: Client,
    close: CloseSignal,
) {
    let outbound_closing = client.outbound_closing();
    loop {
        let msg = tokio::select! {
            biased;
            msg = out_rx.recv() => msg,
            () = outbound_closing.cancelled() => {
                out_rx.close();
                break;
            }
        };
        let Some(msg) = msg else {
            client.close_outbound();
            break;
        };
        if let Err(e) = writer.send(msg).await {
            warn!(error = %e, "send_loop: transport write failed");
            // ADR 0018: the writer reports its terminal failure and performs no
            // cleanup of its own; the engine runs the one close operation.
            close.request(CloseCause::WriterFailed);
            return;
        }
    }
    while let Some(msg) = out_rx.recv().await {
        if let Err(e) = writer.send(msg).await {
            warn!(error = %e, "send_loop: transport write failed while draining");
            close.request(CloseCause::WriterFailed);
            return;
        }
    }
    if let Err(e) = writer.shutdown().await {
        warn!(error = %e, "send_loop: transport shutdown failed");
        close.request(CloseCause::WriterFailed);
    }
}

/// The static registrations and lifecycle callbacks awaiting the initialize
/// transaction. Held only while the connection is [`Lifecycle::Uninitialized`];
/// the transaction consumes it once, so it need not be `Clone`.
struct Pending<S> {
    registrations: Registrations<S>,
    configure_initialize: Option<ConfigureInitialize<S>>,
    on_initialize: Option<OnInitialize<S>>,
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
    lifecycle: Lifecycle<S>,
    inbound: InboundRegistry,
    tasks: TaskGroup<R>,
    out_tx: UnboundedSender<RawMessage>,
    client: Client,
    /// Cancelled once by [`close`](Self::close). Every request-scoped token is
    /// a child of it, so closing the session cancels all outstanding user work
    /// even where the completion gate has already claimed its registry entry.
    session: CancellationToken,
    close: CloseSignal,
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
        out_tx: UnboundedSender<RawMessage>,
        client: Client,
        close: CloseSignal,
        send_task: TaskHandle,
    ) -> Self {
        Self {
            state: server.state,
            documents: Documents::new(),
            workspace: None,
            lifecycle: Lifecycle::Uninitialized(Box::new(Pending {
                registrations: server.registrations,
                configure_initialize: server.configure_initialize,
                on_initialize: server.on_initialize,
                layers: server.layers,
                concurrency_limit: server.concurrency_limit,
            })),
            inbound: InboundRegistry::default(),
            tasks: TaskGroup::new(runtime),
            out_tx,
            client,
            session: CancellationToken::new(),
            close,
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
        loop {
            self.tasks.reap_finished().await;
            let msg = tokio::select! {
                // `biased`: an already-requested close wins over a message that
                // happens to be ready, so the ending stays deterministic.
                biased;
                () = requested.cancelled() => break,
                msg = reader.recv() => msg,
            };

            match msg {
                Ok(msg) => match self.dispatch(msg).await {
                    Flow::Continue => {}
                    Flow::Close(cause) => {
                        self.close.request(cause);
                        break;
                    }
                },
                Err(TransportError::Closed) => {
                    warn!("transport closed by peer before exit notification");
                    self.close.request(CloseCause::ReaderEof);
                    break;
                }
                Err(error) => {
                    self.close.request(CloseCause::ReaderFailed(error));
                    break;
                }
            }
        }

        self.close().await;
        self.close
            .take_cause()
            .expect("every path out of the read-loop records its close cause")
            .into_result()
    }

    async fn dispatch(&mut self, msg: RawMessage) -> Flow {
        match msg {
            RawMessage::Request { id, method, params } => {
                let span = info_span!("request", method = %method, id = ?id);
                // Request tokens descend from the session token, so closing the
                // session cancels in-flight user work even after the completion
                // gate has claimed its registry entry.
                let cancellation = (method != "initialize").then(|| self.session.child_token());
                let Some(reservation) = self.inbound.reserve(id.clone(), cancellation.clone())
                else {
                    enqueue_error(
                        &self.out_tx,
                        id,
                        LspError::invalid_request("duplicate request id"),
                    );
                    return Flow::Continue;
                };

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
                        // The shutdown request answers itself first, so its own
                        // entry is gone before the sweep below; only then does a
                        // successful shutdown cancel the rest of the in-flight
                        // work and enter `ShuttingDown`.
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
                        );
                    }
                }
            }
            RawMessage::Notification { method, params } => match method.as_ref() {
                "exit" => {
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
                        Ok(cancel) => self.inbound.complete_cancellation(&self.out_tx, &cancel.id),
                        Err(error) => {
                            debug!(%error, "ignoring malformed $/cancelRequest");
                        }
                    }
                }
                other => {
                    // Outside the running state only the completion and exit
                    // notifications handled above are processed: before
                    // `initialize` there is no Router, and after `shutdown` the
                    // connection accepts no further user work. While running, a
                    // registered custom notification dispatches with no
                    // response; an unregistered one is ignored.
                    match &self.lifecycle {
                        Lifecycle::Running(service) => {
                            let service = Arc::clone(service);
                            let params = match decode_value(&params) {
                                Ok(params) => params,
                                Err(error) => {
                                    debug!(method = other, %error, "notification params ignored");
                                    return Flow::Continue;
                                }
                            };
                            let span = info_span!("notification", method = %other);
                            let ctx = attach_workspace(
                                Context::for_notification(
                                    span,
                                    self.client.clone(),
                                    self.documents.clone(),
                                ),
                                &self.workspace,
                            );
                            let result = service
                                .call(IncomingCall::notification(
                                    method.into_owned(),
                                    params,
                                    ctx,
                                    Arc::clone(&self.state),
                                ))
                                .await;
                            if !matches!(result, ServiceResult::NoResponse) {
                                warn!("notification service attempted to produce a response");
                            }
                        }
                        _ => debug!(method = other, "notification outside running state ignored"),
                    }
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
                let _ = self.out_tx.send(RawMessage::ProtocolError { error });
            }
        }

        Flow::Continue
    }

    /// Run the one `initialize` transaction (ADR 0017, ADR 0018).
    ///
    /// In order: validate and consume the sole `initialize`; run
    /// `configure_initialize` against a transactional registrar; on success
    /// commit and permanently freeze the Router; establish the `Workspace`,
    /// `Documents` encoding, and generated capabilities; run `on_initialize`
    /// for optional `ServerInfo`; then enter the running state and reply. Any
    /// configuration, validation, or `on_initialize` failure enqueues the fixed
    /// error and requests the terminal close rather than returning to
    /// uninitialized.
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
            configure_initialize,
            on_initialize,
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
            Some(callback) => callback(&params, &mut registrar),
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

        // Establish Workspace, Documents encoding, and generated capabilities
        // from InitializeParams before `on_initialize` observes them. Per
        // ADR 0018's precedence, the Workspace is established (step 4) before
        // protocol-owned fields are negotiated and capabilities generated
        // (step 5).
        let established = Workspace::from_params(&params);
        self.workspace = Some(established.clone());

        let position_encoding = self.documents.negotiate_position_encoding(&params);
        let mut capabilities = router.capabilities();
        capabilities.position_encoding = Some(position_encoding);

        // `on_initialize` may contribute optional ServerInfo but cannot
        // register routes or replace the generated capabilities.
        let server_info = match on_initialize {
            Some(hook) => {
                let ctx = Context::for_request(
                    reservation.id.clone(),
                    span.clone(),
                    self.client.clone(),
                    self.documents.clone(),
                )
                .with_workspace(established);
                match hook(
                    Arc::clone(&self.state),
                    ctx,
                    params,
                    self.session.child_token(),
                )
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
            encode_body(&InitializeResult {
                capabilities,
                server_info,
            }),
        );
        self.lifecycle = Lifecycle::Running(build_service_stack(router, layers, concurrency_limit));
        Flow::Continue
    }

    /// Spawn one user request into the engine's task group, racing user
    /// dispatch against the request's cancellation so a cancelled request stops
    /// at its next yield point, then hand whichever finished first to the
    /// completion gate.
    fn spawn_service_request(
        &mut self,
        service: UserService<S>,
        span: Span,
        reservation: Reservation,
        method: String,
        params: serde_json::Value,
        cancellation: CancellationToken,
    ) {
        let state = Arc::clone(&self.state);
        let documents = self.documents.clone();
        let workspace = self.workspace.clone();
        let out_tx = self.out_tx.clone();
        let client = self.client.clone();
        let inbound = self.inbound.clone();
        self.tasks.spawn(async move {
            let id = reservation.id.clone();
            let ctx = attach_workspace(
                Context::for_request(id.clone(), span, client, documents),
                &workspace,
            )
            .with_cancellation(cancellation.clone());
            let call = IncomingCall::request(method, id, params, ctx, state);
            let result = match select(
                Box::pin(service.call(call)),
                Box::pin(cancellation.cancelled()),
            )
            .await
            {
                Either::Left((result, _)) => result,
                Either::Right(((), _)) => ServiceResult::Error(LspError::RequestCancelled),
            };
            let result = match result {
                ServiceResult::Response(value) => encode_body(&value),
                ServiceResult::Error(error) => Err(error),
                ServiceResult::NoResponse => {
                    Err(LspError::internal("request service returned no response"))
                }
            };
            inbound.complete(&out_tx, reservation, result);
        });
    }

    /// The engine's one close operation (ADR 0018).
    ///
    /// Every close cause runs exactly these steps, in this order, and a second
    /// call is a no-op: new outbound work is rejected, the session is
    /// cancelled, every pending `Client` request is resolved, both registries
    /// are emptied, every handler task is aborted and then joined, and the
    /// outbound queue is closed before the writer task is joined. No task is
    /// detached and no pending `Client` future is left unresolved.
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
        self.tasks.abort_and_join().await;
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
        for handle in self.tasks.handles.iter().chain(self.send_task.iter()) {
            handle.abort();
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

/// Attach the established [`Workspace`] to a handler `Context`, if one exists.
/// Post-initialize dispatch always has one; the fallback keeps the helper total.
fn attach_workspace(ctx: Context, workspace: &Option<Workspace>) -> Context {
    match workspace {
        Some(ws) => ctx.with_workspace(ws.clone()),
        None => ctx,
    }
}

/// Enqueue a success response after the protocol engine's final wire encoding,
/// or enqueue the mapped wire error.
fn enqueue_encoded(
    out_tx: &UnboundedSender<RawMessage>,
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
    let _ = out_tx.send(response);
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

fn enqueue_error(out_tx: &UnboundedSender<RawMessage>, id: RequestId, err: LspError) {
    let _ = out_tx.send(error_response(id, &err));
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let registry = InboundRegistry::default();
        let id = RequestId::Number(2);

        let first = registry
            .reserve(id.clone(), Some(CancellationToken::new()))
            .expect("the id is free");
        assert!(
            registry
                .reserve(id.clone(), Some(CancellationToken::new()))
                .is_none(),
            "an in-flight id is not reserved twice"
        );

        // `$/cancelRequest` claims the gate and answers the first request.
        registry.complete_cancellation(&out_tx, &id);
        // The peer then reuses the id for a new request.
        let second = registry
            .reserve(id.clone(), Some(CancellationToken::new()))
            .expect("the id is free once the first request is answered");

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
    fn only_a_shutdown_exit_reports_code_zero() {
        assert_eq!(Outcome::Exit { code: 0 }.code(), 0);
        assert_eq!(Outcome::Exit { code: 1 }.code(), 1);
        assert_eq!(Outcome::TransportClosed.code(), 1);
        assert_eq!(Outcome::WriterFailed.code(), 1);
        assert_eq!(Outcome::InitializeFailed.code(), 1);
    }
}
