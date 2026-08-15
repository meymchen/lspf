//! Connection-owned protocol engine for the 0.2 `Server<S>`.
//!
//! This slice serves a connection end to end for the lifecycle plus typed
//! custom requests, notifications, and commands. `initialize` is the one
//! bounded transaction that can conditionally extend the Router, freeze it,
//! generate capabilities, establish the connection's [`Workspace`],
//! [`Documents`], and negotiated position encoding, and run the
//! `on_initialize` lifecycle hook — all without exposing partial state
//! (ADR 0017, ADR 0018). The later lifecycle notifications carry the remaining
//! hooks: the client's `initialized` runs `on_initialized` once, in the
//! running state only, and the peer's `exit` runs `on_exit` before the engine
//! computes the exit outcome (ADR 0024) — the hook resolves to `()`, so it
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

use bytes::Bytes;
use futures_util::future::{Either, select};
use lsp_types::{
    DidChangeConfigurationParams, DidChangeTextDocumentParams, DidChangeWorkspaceFoldersParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    InitializeParams, InitializedParams, OneOf, ServerInfo, SetTraceParams, TextDocumentSyncKind,
    TextDocumentSyncOptions, TextDocumentSyncSaveOptions, WillSaveTextDocumentParams,
    WorkDoneProgressCancelParams, WorkspaceFoldersServerCapabilities, WorkspaceServerCapabilities,
};
use serde::Serialize;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, debug, info_span, warn};

use crate::builder::{
    ConfigureInitialize, InitializeRegistrar, OnExit, OnInitialize, OnInitialized,
    ProtocolNotification, Registrations, Server,
};
use crate::capability::GeneratedCapabilities;
use crate::client::{Client, OutboundQueue, OutboundRegistry};
use crate::codec::{decode_params, decode_value, encode_body};
use crate::context::Context;
use crate::documents::Documents;
use crate::error::Error;
use crate::file_provider::SharedFileProvider;
use crate::progress::{ProgressCancel, ProgressRegistry};
use crate::raw::{JsonRpcError, RawMessage, RequestId};
use crate::runtime::{Runtime, TaskHandle, TaskSend, default_runtime};
use crate::service::{IncomingCall, ServiceResult, UserLayer, UserService, build_service_stack};
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
    let (out_tx, out_rx) = OutboundQueue::new(server.outbound_warning_threshold);
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
        out_tx: &OutboundQueue,
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

    fn complete_cancellation(&self, out_tx: &OutboundQueue, id: &RequestId) {
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
    fn cancel_all_with_response(&self, out_tx: &OutboundQueue) {
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
        // The depth counts what is still queued, so each message is decremented
        // once its transport send has succeeded or failed — including the
        // terminally failed send, after which the loop returns.
        let sent = writer.send(msg).await;
        client.record_done();
        if let Err(e) = sent {
            warn!(error = %e, "send_loop: transport write failed");
            // ADR 0018: the writer reports its terminal failure and performs no
            // cleanup of its own; the engine runs the one close operation.
            close.request(CloseCause::WriterFailed);
            return;
        }
    }
    while let Some(msg) = out_rx.recv().await {
        let sent = writer.send(msg).await;
        client.record_done();
        if let Err(e) = sent {
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
    file_provider: SharedFileProvider,
    configure_initialize: Option<ConfigureInitialize<S>>,
    on_initialize: Option<OnInitialize<S>>,
    on_initialized: Option<OnInitialized<S>>,
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
    /// The `on_exit` hook awaiting the peer's `exit` notification. Lifted out
    /// of [`Pending`] when the initialize transaction succeeds; an `exit`
    /// received earlier closes without a Workspace to hand it.
    on_exit: Option<OnExit<S>>,
    inbound: InboundRegistry,
    tasks: TaskGroup<R>,
    out_tx: OutboundQueue,
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
        out_tx: OutboundQueue,
        client: Client,
        close: CloseSignal,
        send_task: TaskHandle,
    ) -> Self {
        Self {
            state: server.state,
            documents: Documents::new(),
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
                on_exit: server.on_exit,
                layers: server.layers,
                concurrency_limit: server.concurrency_limit,
            })),
            on_initialized: None,
            on_exit: None,
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
                        let span = info_span!("notification", method = "exit");
                        let ctx = Context::for_notification(
                            span,
                            self.client.clone(),
                            self.established_workspace(),
                        );
                        hook(Arc::clone(&self.state), ctx).await;
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
                        Ok(cancel) => self.inbound.complete_cancellation(&self.out_tx, &cancel.id),
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
                    let span = info_span!("notification", method = "initialized");
                    let ctx = Context::for_notification(
                        span,
                        self.client.clone(),
                        self.established_workspace(),
                    );
                    hook(Arc::clone(&self.state), ctx, params).await;
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
                let _ = self.out_tx.send(RawMessage::ProtocolError { error });
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
        let span = info_span!("notification", method = %method);
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
                self.documents.open(params.text_document);
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
    /// `on_initialized` and `on_exit` hooks for the running state; run
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

        // The post-initialize lifecycle hooks become reachable only through
        // dispatch in the running state, which the transaction enters at its
        // end; they stay parked here until then.
        self.on_initialized = on_initialized;
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
            encode_body(&WireInitializeResult {
                capabilities,
                server_info,
            }),
        );
        self.lifecycle = Lifecycle::Running(build_service_stack(router, layers, concurrency_limit));
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
        let workspace = self.established_workspace();
        let out_tx = self.out_tx.clone();
        let client = self.client.clone();
        let inbound = self.inbound.clone();
        self.tasks.spawn(async move {
            let id = reservation.id.clone();
            let ctx = Context::for_request(id.clone(), span, client, workspace)
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

fn enqueue_error(out_tx: &OutboundQueue, id: RequestId, err: LspError) {
    let _ = out_tx.send(error_response(id, &err));
}

#[cfg(test)]
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

    /// A writer that records what it sent and fails the `fail_on_send`-th send
    /// (1-based; `None` never fails), driving the send-loop through its
    /// success, draining, and terminal-failure paths.
    struct ScriptedWriter {
        outbox: Arc<Mutex<Vec<RawMessage>>>,
        fail_on_send: Option<usize>,
        sends: usize,
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
        let client = Client::new(queue.clone(), OutboundRegistry::default());

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
    async fn the_send_loop_decrements_each_attempted_send_including_the_failed_one() {
        let (queue, rx) = OutboundQueue::new(16);
        let client = Client::new(queue.clone(), OutboundRegistry::default());
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
            1,
            "the succeeded and the terminally failed send are both decremented"
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
        let client = Client::new(queue.clone(), OutboundRegistry::default());
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
}
