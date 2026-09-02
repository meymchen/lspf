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

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
#[cfg(all(test, not(target_arch = "wasm32")))]
use futures_channel::mpsc::UnboundedReceiver;
use gen_lsp_types::{
    DidChangeConfigurationParams, DidChangeTextDocumentParams, DidChangeWorkspaceFoldersParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    InitializeParams, InitializedParams, ProgressToken, Save, ServerInfo, SetTraceParams,
    TextDocumentSyncKind, TextDocumentSyncOptions, TraceValue, WillSaveTextDocumentParams,
    WorkDoneProgressCancelParams, WorkspaceFoldersServerCapabilities,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, debug, warn};

use crate::builder::{
    ConfigureInitialize, InitializeRegistrar, OnExit, OnInitialize, OnInitialized, OnShutdown,
    ProtocolNotification, Registrations, Server,
};
use crate::capability::GeneratedCapabilities;
use crate::client::ClientHandle;
use crate::codec::{decode_params, decode_value, encode_body, request_token};
use crate::context::ServerContext;
use crate::documents::{DocumentMutationError, Documents};
use crate::error::Error;
use crate::failure::{ConnectionDirection, ConnectionFailureCategory, FailureReporter};
use crate::file_provider::SharedFileProvider;
use crate::notebooks::Notebooks;
use crate::progress::{ProgressCancel, ProgressRegistry};
use crate::raw::{RawMessage, RequestId};
use crate::runtime::{Runtime, default_runtime, ensure_runtime_available};
use crate::service::{
    HandlerTimeout, IncomingCall, ServiceResult, UserLayer, UserService, build_service_stack,
};
#[cfg(all(test, not(target_arch = "wasm32")))]
use crate::session::send_loop as drive_send_loop;
#[cfg(all(test, not(target_arch = "wasm32")))]
use crate::session::{CloseSignal, OutboundQueue, OutboundRegistry, enqueue_encoded};
use crate::session::{
    INBOUND_CAPACITY_EXHAUSTED, InboundReserveError, ProtocolSession, Reservation, SessionInput,
    run_handler_with_deadline,
};
use crate::telemetry::{Completion, ConnectionTrace, Direction, Instant};
#[cfg(all(test, not(target_arch = "wasm32")))]
use crate::transport::TransportWriter;
use crate::transport::{Transport, TransportError, TransportReader};
use crate::workspace::Workspace;
#[cfg(all(test, not(target_arch = "wasm32")))]
use std::sync::Mutex;

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
    changes: &[gen_lsp_types::TextDocumentContentChangeEvent],
) -> std::result::Result<(), LspError> {
    if kind == TextDocumentSyncKind::Incremental {
        return Ok(());
    }
    if kind == TextDocumentSyncKind::Full {
        return if changes.iter().all(|change| {
            matches!(
                change,
                gen_lsp_types::TextDocumentContentChangeEvent::TextDocumentContentChangeWholeDocument(_)
            )
        }) {
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
/// Serving a Server or Client connection resolves to exactly one `Outcome` or
/// to a transport [`Error`]; it never terminates the process. A server binary
/// maps its outcome to a process disposition itself — [`Outcome::code`]
/// reports the exit code the LSP lifecycle implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The endpoint processed `exit`. For a Server, `code` is 0 when the peer
    /// sent `shutdown` first and 1 otherwise; a Client permits local `exit`
    /// only after its `shutdown` request succeeds and therefore reports 0.
    Exit {
        /// The process exit code prescribed by the LSP lifecycle.
        code: i32,
    },
    /// The transport ended before `exit`, or a Client disconnected locally.
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
    fn writer_failed() -> Self {
        Self::WriterFailed
    }

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

/// Drive a [`Server`] over `transport` until the peer exits, the transport
/// closes, a transport error ends the session, or a failed initialize
/// transaction enters the terminal close path.
pub(crate) async fn run<S, T>(server: Server<S>, transport: T) -> Result<Outcome>
where
    S: Send + Sync + 'static,
    T: Transport,
{
    ensure_runtime_available()?;
    let connection_trace = ConnectionTrace::new();
    let failure_reporter = FailureReporter::new(server.error_hook.clone(), connection_trace.id());
    let connection_span = connection_trace.span();
    let (reader, writer) = transport.split();
    let runtime = default_runtime();
    let (protocol, client) = ProtocolSession::start(
        runtime,
        server.resource_policy,
        writer,
        connection_trace,
        connection_span.clone(),
        failure_reporter.clone(),
        CloseCause::writer_failed,
        ClientHandle::new,
    );
    ProtocolEngine::new(server, protocol, client, connection_trace, failure_reporter)
        .serve(reader)
        .instrument(connection_span)
        .await
}

#[derive(serde::Deserialize)]
struct CancelParams {
    id: RequestId,
}

/// Whether a protocol built-in's post-validation hook runs.
///
/// Most built-ins gate their hook on the `Result` of
/// [`ProtocolEngine::process_protocol_notification`]; the work-done progress
/// cancel built-in reports its own non-error registry misses at debug level
/// and signals malformed parameters through the gate directly instead.
enum BuiltInGate {
    /// Decode — and any mutation — succeeded: dispatch the registered hook.
    RunHook,
    /// Parameters violated the protocol contract: report and skip the hook.
    ProtocolFailure,
}

enum BuiltInError {
    Protocol(LspError),
    Overload(LspError),
}

impl From<LspError> for BuiltInError {
    fn from(error: LspError) -> Self {
        Self::Protocol(error)
    }
}

impl From<DocumentMutationError> for BuiltInError {
    fn from(error: DocumentMutationError) -> Self {
        match error {
            DocumentMutationError::Capacity(error) => Self::Overload(error),
            DocumentMutationError::Protocol(error) => Self::Protocol(error),
        }
    }
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
            return BuiltInGate::ProtocolFailure;
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
    client: ClientHandle,
    close: CloseSignal<CloseCause>,
) {
    drive_send_loop(writer, out_rx, client, close, CloseCause::writer_failed).await;
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
    notebooks: Notebooks,
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
    protocol: ProtocolSession<R, ClientHandle, CloseCause>,
    client: ClientHandle,
    trace: ConnectionTrace,
    failure_reporter: FailureReporter,
}

impl<S, R> ProtocolEngine<S, R>
where
    S: Send + Sync + 'static,
    R: Runtime,
{
    fn new(
        server: Server<S>,
        protocol: ProtocolSession<R, ClientHandle, CloseCause>,
        client: ClientHandle,
        trace: ConnectionTrace,
        failure_reporter: FailureReporter,
    ) -> Self {
        let max_inbound_requests = server.resource_policy.max_inbound_requests;
        Self {
            state: server.state,
            documents: Documents::with_resource_policy(server.resource_policy, trace),
            notebooks: Notebooks::default(),
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
            protocol,
            client,
            trace,
            failure_reporter,
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
        loop {
            let msg = match self.protocol.next_input(&mut reader).await {
                SessionInput::CloseRequested => break,
                SessionInput::OutboundFailed => {
                    self.protocol.request_close(CloseCause::WriterFailed);
                    break;
                }
                SessionInput::Message(message) => message,
            };

            match msg {
                Ok(msg) => {
                    self.trace.message(Direction::Inbound, &msg);
                    match self.dispatch(msg).await {
                        Flow::Continue => {}
                        Flow::Close(cause) => {
                            self.protocol.request_close(cause);
                            break;
                        }
                    }
                }
                Err(TransportError::Closed) => {
                    warn!("transport closed by peer before exit notification");
                    self.protocol.request_close(CloseCause::ReaderEof);
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
                    self.protocol.request_close(CloseCause::ReaderFailed(error));
                    break;
                }
            }
        }

        self.close().await;
        let cause = self.protocol.final_close_cause();
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
                let reserved = match self.protocol.reserve_inbound(
                    id.clone(),
                    method.as_ref(),
                    method != "initialize",
                ) {
                    Ok(reserved) => reserved,
                    Err(InboundReserveError::DuplicateId) => {
                        self.failure_reporter.report_unvalidated_inbound_method(
                            ConnectionFailureCategory::Protocol,
                            Some(&id),
                        );
                        self.trace.request_completed(
                            method.as_ref(),
                            &id,
                            Instant::now(),
                            Direction::Inbound,
                            Completion::Rejected,
                        );
                        self.protocol
                            .reject_inbound(id, LspError::invalid_request("duplicate request id"));
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
                        self.protocol.reject_inbound(
                            id,
                            LspError::ServerCancelled(INBOUND_CAPACITY_EXHAUSTED.to_string()),
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
                    self.failure_reporter.report_unvalidated_inbound_method(
                        ConnectionFailureCategory::Protocol,
                        Some(&reservation.id),
                    );
                    self.protocol
                        .complete_inbound(reservation, Err(LspError::ServerNotInitialized));
                    return Flow::Continue;
                }
                // After `shutdown`, every request is invalid until `exit`.
                if matches!(self.lifecycle, Lifecycle::ShuttingDown | Lifecycle::Exited) {
                    self.failure_reporter.report_unvalidated_inbound_method(
                        ConnectionFailureCategory::Protocol,
                        Some(&reservation.id),
                    );
                    self.protocol.complete_inbound(
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
                            self.protocol.complete_inbound(reservation, Err(err));
                            return Flow::Continue;
                        }
                        if let Some(hook) = &self.on_shutdown {
                            let cancellation = cancellation
                                .expect("shutdown is a cancellable non-initialize request");
                            let ctx = ServerContext::for_request(
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
                                self.protocol.complete_inbound(reservation, Err(err));
                                return Flow::Continue;
                            }
                        }
                        // The successful shutdown request answers itself first,
                        // so its own entry is gone before the sweep below; only
                        // then cancel the rest of the in-flight work and enter
                        // `ShuttingDown`.
                        self.protocol
                            .complete_inbound(reservation, encode_body(&serde_json::Value::Null));
                        self.protocol.cancel_all_inbound_with_response();
                        self.lifecycle = Lifecycle::ShuttingDown;
                    }
                    _other => {
                        // Precedence guarantees the connection is running here.
                        let service = match &self.lifecycle {
                            Lifecycle::Running(service) => Arc::clone(service),
                            _ => {
                                self.protocol.complete_inbound(
                                    reservation,
                                    Err(LspError::ServerNotInitialized),
                                );
                                return Flow::Continue;
                            }
                        };
                        let params = match decode_value(&params) {
                            Ok(params) => params,
                            Err(error) => {
                                self.failure_reporter.report_unvalidated_inbound_method(
                                    ConnectionFailureCategory::Protocol,
                                    Some(&reservation.id),
                                );
                                self.protocol.complete_inbound(reservation, Err(error));
                                return Flow::Continue;
                            }
                        };
                        let work_done_token =
                            match request_token::<ProgressToken>(&params, "workDoneToken") {
                                Ok(token) => token,
                                Err(error) => {
                                    self.protocol.complete_inbound(
                                        reservation,
                                        Err(LspError::invalid_params(error)),
                                    );
                                    return Flow::Continue;
                                }
                            };
                        let partial_result_token =
                            if crate::partial_result::supports_method(method.as_ref()) {
                                match request_token::<ProgressToken>(&params, "partialResultToken")
                                {
                                    Ok(token) => token,
                                    Err(error) => {
                                        self.protocol.complete_inbound(
                                            reservation,
                                            Err(LspError::invalid_params(error)),
                                        );
                                        return Flow::Continue;
                                    }
                                }
                            } else {
                                None
                            };
                        let method = method.into_owned();
                        let ctx = ServerContext::for_request(
                            reservation.id.clone(),
                            span.clone(),
                            self.client.clone(),
                            self.established_workspace(),
                        )
                        .with_cancellation(
                            cancellation
                                .as_ref()
                                .expect("non-initialize requests are cancellable")
                                .clone(),
                        )
                        .with_work_done_token(work_done_token)
                        .with_partial_result(method.clone(), partial_result_token);
                        self.spawn_service_request(
                            service,
                            reservation,
                            method,
                            params,
                            ctx,
                            cancellation.expect("non-initialize requests are cancellable"),
                            self.protocol.handler_timeout(),
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
                    // receives only the shared state and a live `ServerContext`.
                    let established = matches!(
                        self.lifecycle,
                        Lifecycle::Running(_) | Lifecycle::ShuttingDown
                    );
                    if !established {
                        self.failure_reporter.report(
                            ConnectionFailureCategory::Protocol,
                            Some(ConnectionDirection::Inbound),
                            Some("exit"),
                            None,
                        );
                    }
                    if established && let Some(hook) = self.on_exit.take() {
                        let span = self.trace.notification_span("exit");
                        let ctx = ServerContext::for_notification(
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
                            if let Some(reservation) = self.protocol.cancel_inbound(&cancel.id) {
                                self.protocol.complete_cancelled(reservation);
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
                "initialized" => {
                    // The initialized hook runs at most once, and only after a
                    // successful initialize transaction: outside the running
                    // state there is no Workspace for its ServerContext, and the
                    // notification is ignored without consuming the hook, so a
                    // later, valid `initialized` still runs it. The params are
                    // decoded before the hook is taken, so a malformed
                    // notification leaves it in place too.
                    let Lifecycle::Running(_) = &self.lifecycle else {
                        self.failure_reporter.report(
                            ConnectionFailureCategory::Protocol,
                            Some(ConnectionDirection::Inbound),
                            Some("initialized"),
                            None,
                        );
                        debug!("initialized notification outside the running state ignored");
                        return Flow::Continue;
                    };
                    let params = match decode_initialized_params(&params) {
                        Ok(params) => params,
                        Err(error) => {
                            self.failure_reporter.report(
                                ConnectionFailureCategory::Protocol,
                                Some(ConnectionDirection::Inbound),
                                Some("initialized"),
                                None,
                            );
                            warn!(%error, "dropping initialized notification with malformed params");
                            return Flow::Continue;
                        }
                    };
                    let Some(hook) = self.on_initialized.take() else {
                        return Flow::Continue;
                    };
                    let span = self.trace.notification_span("initialized");
                    let ctx = ServerContext::for_notification(
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
                        self.failure_reporter.report_unvalidated_inbound_method(
                            ConnectionFailureCategory::Protocol,
                            None,
                        );
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
                            Ok(BuiltInGate::ProtocolFailure) => {
                                self.failure_reporter.report(
                                    ConnectionFailureCategory::Protocol,
                                    Some(ConnectionDirection::Inbound),
                                    Some(other),
                                    None,
                                );
                                return Flow::Continue;
                            }
                            Err(error) => {
                                let (category, error) = match error {
                                    BuiltInError::Protocol(error) => {
                                        (ConnectionFailureCategory::Protocol, error)
                                    }
                                    BuiltInError::Overload(error) => {
                                        (ConnectionFailureCategory::Overload, error)
                                    }
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
                            self.failure_reporter.report_unvalidated_inbound_method(
                                ConnectionFailureCategory::Protocol,
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
                    self.failure_reporter.report(
                        ConnectionFailureCategory::Protocol,
                        Some(ConnectionDirection::Inbound),
                        None,
                        Some(&id),
                    );
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
                self.protocol.send_protocol_error(error);
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
        let ctx = ServerContext::for_notification(
            span,
            self.client.clone(),
            self.established_workspace(),
        );
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
    ) -> std::result::Result<BuiltInGate, BuiltInError> {
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
                        .unwrap_or(TextDocumentSyncKind::Incremental),
                    &params.content_changes,
                )?;
                self.documents.apply_changes(
                    &params.text_document.text_document_identifier.uri,
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
                    Some(Save::SaveOptions(options))
                        if options.include_text == Some(true)
                ) && params.text.is_none()
                {
                    return Err(LspError::invalid_request(
                        "didSave text is required by textDocumentSync.save.includeText",
                    )
                    .into());
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
                if matches!(&params.value, TraceValue::Custom(_)) {
                    return Err(LspError::invalid_params(
                        "setTrace value must be off, messages, or verbose",
                    )
                    .into());
                }
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
            _ if self.document_sync.change == Some(TextDocumentSyncKind::None) => false,
            ProtocolNotification::Open | ProtocolNotification::Close => {
                self.document_sync.open_close == Some(true)
            }
            ProtocolNotification::Change => matches!(
                self.document_sync.change,
                Some(TextDocumentSyncKind::Full | TextDocumentSyncKind::Incremental)
            ),
            ProtocolNotification::WillSave => self.document_sync.will_save == Some(true),
            ProtocolNotification::Save => matches!(
                self.document_sync.save,
                Some(Save::Bool(true)) | Some(Save::SaveOptions(_))
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
            self.failure_reporter.report(
                ConnectionFailureCategory::Protocol,
                Some(ConnectionDirection::Inbound),
                Some("initialize"),
                Some(&reservation.id),
            );
            self.protocol.complete_inbound(
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
                self.protocol.complete_inbound(reservation, Err(err));
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
                self.protocol.complete_inbound(
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
        // Documents and Notebooks handles; the engine keeps its own clones for
        // built-in synchronization mutations.
        let established = Workspace::from_params_with_stores_and_provider(
            &params,
            self.documents.clone(),
            self.notebooks.clone(),
            file_provider,
            self.client.shared_trace(),
        );
        let work_done_token = params.work_done_progress_params.work_done_token.clone();
        self.workspace = Some(established.clone());

        let position_encoding = self.documents.negotiate_position_encoding(&params);
        let mut capabilities = router.generated_capabilities();
        let standard_capabilities = &mut capabilities;
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
        let mut workspace = standard_capabilities.workspace.take().unwrap_or_default();
        workspace.workspace_folders = Some(WorkspaceFoldersServerCapabilities {
            supported: Some(true),
            change_notifications: Some(true.into()),
        });
        standard_capabilities.workspace = Some(workspace);

        // `on_initialize` may contribute optional ServerInfo but cannot
        // register routes or replace the generated capabilities.
        let server_info = match on_initialize {
            Some(hook) => {
                let ctx = ServerContext::for_request(
                    reservation.id.clone(),
                    span.clone(),
                    self.client.clone(),
                    established,
                )
                .with_work_done_token(work_done_token);
                match hook
                    .invoke((
                        Arc::clone(&self.state),
                        ctx,
                        params,
                        self.protocol.cancellation_child(),
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
                        self.protocol.complete_inbound(reservation, Err(err));
                        return Flow::Close(CloseCause::InitializeFailed);
                    }
                }
            }
            None => None,
        };

        self.protocol.complete_inbound(
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
        reservation: Reservation,
        method: String,
        params: serde_json::Value,
        ctx: ServerContext,
        cancellation: CancellationToken,
        default_timeout: Duration,
    ) {
        let state = Arc::clone(&self.state);
        let completion_gate = self.protocol.completion_gate();
        let permit = Arc::clone(&reservation._permit);
        let trace = self.trace;
        self.protocol.spawn(
            async move {
                let id = reservation.id.clone();
                let trace_id = id.clone();
                let trace_method = method.clone();
                let handler_timeout = HandlerTimeout::new(
                    default_timeout,
                    trace,
                    trace_method.clone(),
                    trace_id.clone(),
                );
                let call =
                    IncomingCall::request(method, id, params, ctx, state, handler_timeout.clone());
                let partial_result_scope = call.context().partial_result_scope();
                let result =
                    run_handler_with_deadline(service.call(call), cancellation, handler_timeout)
                        .await;
                if let Some(scope) = partial_result_scope {
                    scope.finish();
                }
                let result = match result {
                    ServiceResult::Response(value) => encode_body(&value),
                    ServiceResult::Error(error) => Err(error),
                    ServiceResult::NoResponse => {
                        Err(LspError::internal("request service returned no response"))
                    }
                };
                completion_gate.complete(reservation, result);
            },
            permit,
        );
    }

    /// The engine's one close operation (ADR 0018).
    ///
    /// Every close cause runs exactly these steps, in this order, and a second
    /// call is a no-op: new outbound work is rejected, the session is
    /// cancelled, every pending `ClientHandle` request is resolved, the inbound,
    /// outbound, and progress registries are emptied, every handler task is
    /// aborted and then joined, and the outbound queue is closed before the
    /// writer task is joined. No task is detached and no pending `ClientHandle`
    /// future is left unresolved.
    async fn close(&mut self) {
        if matches!(self.lifecycle, Lifecycle::Exited) {
            return;
        }
        self.lifecycle = Lifecycle::Exited;
        self.protocol.close().await;
        // Documents are Server endpoint state, so their lifecycle stays out of
        // the shared session even though close releases retained snapshots.
        self.documents.clear();
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
#[cfg(all(test, not(target_arch = "wasm32")))]
fn final_close_cause(close: &CloseSignal<CloseCause>, out_tx: &OutboundQueue) -> CloseCause {
    let recorded = close
        .take_cause()
        .expect("every path out of the read-loop records its close cause");
    if out_tx.failure().is_cancelled() {
        CloseCause::WriterFailed
    } else {
        recorded
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use gen_lsp_types::ProgressToken;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::session::{InboundRegistry, TaskGroup};

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
        registry.register(ProgressToken::Int(1), true, cancellation.clone());

        let (gate, events) = gated_cancel(&registry, br#"{"token": 1}"#);

        assert!(
            matches!(gate, BuiltInGate::RunHook),
            "a successful decode lets the hook run"
        );
        assert!(cancellation.is_cancelled());
        assert!(
            registry.is_active(&ProgressToken::Int(1)),
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
        registry.register(ProgressToken::Int(1), false, plain.clone());

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
            registry.is_active(&ProgressToken::Int(1)),
            "an unknown token leaves the registry untouched"
        );
    }

    #[test]
    fn malformed_cancel_params_log_at_debug_and_close_the_hook_gate() {
        let registry = ProgressRegistry::default();
        let cancellation = CancellationToken::new();
        registry.register(ProgressToken::Int(1), true, cancellation.clone());

        for raw in [
            br#"{"token": true}"#.as_slice(),
            br#"{}"#.as_slice(),
            b"not json".as_slice(),
        ] {
            let (gate, events) = gated_cancel(&registry, raw);
            assert!(
                matches!(gate, BuiltInGate::ProtocolFailure),
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

    fn content_change(with_range: bool) -> gen_lsp_types::TextDocumentContentChangeEvent {
        if with_range {
            gen_lsp_types::TextDocumentContentChangePartial::new(
                gen_lsp_types::Range {
                    start: gen_lsp_types::Position::new(0, 0),
                    end: gen_lsp_types::Position::new(0, 1),
                },
                None,
                "replacement".to_string(),
            )
            .into()
        } else {
            gen_lsp_types::TextDocumentContentChangeWholeDocument {
                text: "replacement".to_string(),
            }
            .into()
        }
    }

    #[test]
    fn sync_kind_validation_accepts_only_compatible_change_shapes() {
        assert!(
            validate_sync_changes(TextDocumentSyncKind::Full, &[content_change(false)]).is_ok()
        );
        assert!(
            validate_sync_changes(TextDocumentSyncKind::Full, &[content_change(true)]).is_err()
        );
        assert!(
            validate_sync_changes(
                TextDocumentSyncKind::Incremental,
                &[content_change(true), content_change(false)],
            )
            .is_ok()
        );
        assert!(
            validate_sync_changes(TextDocumentSyncKind::None, &[content_change(false)]).is_err()
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
        let client = ClientHandle::new(queue.clone(), OutboundRegistry::default(), None);
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
