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
//! cannot change the exit code the lifecycle implies. The protocol session
//! admits inbound requests before the engine sees them, and its atomic
//! completion gate arbitrates success, errors, and cancellation (ADR 0037).
//!
//! Every way a connection can end — reader EOF, a reader error, a writer send
//! or shutdown failure, `exit`, and the fatal termination a failed initialize
//! transaction takes — requests the same idempotent close operation. The first
//! requester records its cause and wakes the read loop; the engine then
//! performs the cleanup exactly once and reports the selected [`SessionEnd`] as
//! an [`Outcome`] or a transport [`Error`]. The engine never terminates the
//! process; the entry point decides what an [`Outcome`] means for a binary.

use std::sync::Arc;

use bytes::Bytes;
use gen_lsp_types::{
    DidChangeConfigurationParams, DidChangeNotebookDocumentParams, DidChangeTextDocumentParams,
    DidChangeWorkspaceFoldersParams, DidCloseNotebookDocumentParams, DidCloseTextDocumentParams,
    DidOpenNotebookDocumentParams, DidOpenTextDocumentParams, DidSaveNotebookDocumentParams,
    DidSaveTextDocumentParams, InitializeParams, InitializedParams, ProgressToken, Save,
    ServerInfo, SetTraceParams, TextDocumentSyncKind, TextDocumentSyncOptions, TraceValue,
    WillSaveTextDocumentParams, WorkDoneProgressCancelParams, WorkspaceFoldersServerCapabilities,
};
use serde::Serialize;
use tracing::{Instrument, debug, warn};

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
use crate::notebooks::{NotebookMutationError, Notebooks};
use crate::partial_result::PartialResultScope;
use crate::progress::{ProgressCancel, ProgressRegistry};
use crate::runtime::{Runtime, default_runtime, ensure_runtime_available};
use crate::service::{IncomingCall, ServiceResult, UserLayer, UserService, build_service_stack};
use crate::session::{AdmittedRequest, EndpointCause, ProtocolSession, SessionEnd, SessionEvent};
use crate::telemetry::ConnectionTrace;
use crate::transport::{Transport, TransportReader};
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

/// The Server endpoint's own reasons to close. Reader, writer, and
/// required-admission failures are the session's [`SessionEnd`] variants.
#[derive(Debug)]
enum CloseCause {
    /// An `exit` notification was processed; carries the LSP exit code.
    Exit { code: i32 },
    /// A failed initialize transaction terminated the connection (ADR 0018).
    InitializeFailed,
}

impl EndpointCause for CloseCause {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Exit { .. } => "exit",
            Self::InitializeFailed => "initialize_failed",
        }
    }
}

/// Map the selected ending onto what serving the connection returns.
fn into_result(end: SessionEnd<CloseCause>) -> Result<Outcome> {
    match end {
        SessionEnd::Endpoint(CloseCause::Exit { code }) => Ok(Outcome::Exit { code }),
        SessionEnd::Endpoint(CloseCause::InitializeFailed) => Ok(Outcome::InitializeFailed),
        SessionEnd::ReaderEof => Ok(Outcome::TransportClosed),
        SessionEnd::ReaderFailed(error) => Err(Error::Transport(error)),
        SessionEnd::WriterFailed => Ok(Outcome::WriterFailed),
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
        ClientHandle::new,
    );
    ProtocolEngine::new(server, protocol, client, connection_trace, failure_reporter)
        .serve(reader)
        .instrument(connection_span)
        .await
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

impl From<NotebookMutationError> for BuiltInError {
    fn from(error: NotebookMutationError) -> Self {
        match error {
            NotebookMutationError::Protocol(error) => Self::Protocol(error),
            NotebookMutationError::Capacity(error) => Self::Overload(error),
        }
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
    /// Whether this connection advertised `notebookDocumentSync`, which is what
    /// makes the four notebook built-ins reachable.
    notebook_sync: bool,
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
        let documents = Documents::with_resource_policy(server.resource_policy, trace);
        let notebooks =
            Notebooks::with_resource_policy(documents.clone(), server.resource_policy, trace);
        Self {
            state: server.state,
            documents,
            notebooks,
            workspace: None,
            // Document notifications are processed only after initialize has
            // replaced this with the validated effective configuration.
            document_sync: TextDocumentSyncOptions::default(),
            // Likewise: notebook notifications stay unreachable until
            // initialize reads the frozen registrations' opt-in.
            notebook_sync: false,
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

    /// Pull events from the session until some cause requests closure, then
    /// run the one close operation and report the ending.
    ///
    /// The session also wakes on the close signal, so a writer failure ends the
    /// connection without waiting for the peer to send another message.
    async fn serve<Rd>(mut self, mut reader: Rd) -> Result<Outcome>
    where
        Rd: TransportReader,
    {
        loop {
            let flow = match self.protocol.next_event(&mut reader).await {
                SessionEvent::Request(request) => self.dispatch_request(request).await,
                SessionEvent::Notification { method, params } => {
                    self.handle_notification(&method, params).await
                }
                SessionEvent::Closed => break,
            };
            if let Flow::Close(cause) = flow {
                self.protocol.request_close(cause);
            }
        }
        into_result(self.close().await)
    }

    /// Apply lifecycle precedence to one admitted request, then answer it
    /// inline or spawn it through the Service stack.
    async fn dispatch_request(&mut self, request: AdmittedRequest) -> Flow {
        // Initialize precedence: until `initialize` completes, refuse every
        // other request with `ServerNotInitialized`.
        if request.method() != "initialize"
            && matches!(
                self.lifecycle,
                Lifecycle::Uninitialized(_) | Lifecycle::Initializing
            )
        {
            self.failure_reporter.report_unvalidated_inbound_method(
                ConnectionFailureCategory::Protocol,
                Some(request.id()),
            );
            request.respond(Err(LspError::ServerNotInitialized));
            return Flow::Continue;
        }
        // After `shutdown`, every request is invalid until `exit`.
        if matches!(self.lifecycle, Lifecycle::ShuttingDown | Lifecycle::Exited) {
            self.failure_reporter.report_unvalidated_inbound_method(
                ConnectionFailureCategory::Protocol,
                Some(request.id()),
            );
            request.respond(Err(LspError::invalid_request("invalid request")));
            return Flow::Continue;
        }

        match request.method() {
            "initialize" => self.initialize(request).await,
            "shutdown" => {
                self.shutdown(request).await;
                Flow::Continue
            }
            _ => {
                self.spawn_service_request(request);
                Flow::Continue
            }
        }
    }

    async fn shutdown(&mut self, request: AdmittedRequest) {
        let params = request.params();
        let params_result = if params.is_empty() {
            Ok(())
        } else {
            decode_params::<()>(params)
        };
        if let Err(err) = params_result {
            self.failure_reporter.report(
                ConnectionFailureCategory::Protocol,
                Some(ConnectionDirection::Inbound),
                Some("shutdown"),
                Some(request.id()),
            );
            request.respond(Err(err));
            return;
        }
        if let Some(hook) = &self.on_shutdown {
            let cancellation = request.cancellation();
            let span = request.span().clone();
            let ctx = ServerContext::for_request(
                request.id().clone(),
                span.clone(),
                self.client.clone(),
                self.established_workspace(),
            )
            .with_cancellation(cancellation.clone());
            if let Err(err) = hook
                .invoke((Arc::clone(&self.state), ctx, (), cancellation))
                .instrument(span)
                .await
            {
                request.respond(Err(err));
                return;
            }
        }
        // The successful shutdown request answers itself first, so its own
        // entry is gone before the sweep below; only then cancel the rest of
        // the in-flight work and enter `ShuttingDown`.
        request.respond(encode_body(&serde_json::Value::Null));
        self.protocol.cancel_all_inbound_with_response();
        self.lifecycle = Lifecycle::ShuttingDown;
    }

    /// Process one notification the session hands over. Outside the running
    /// state only the lifecycle notifications are processed.
    async fn handle_notification(&mut self, method: &str, params: Bytes) -> Flow {
        match method {
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
    /// Built-in validation is what the stores themselves can establish: a
    /// change names a document that must already be open, each of its ranges
    /// must be applicable under the negotiated encoding, and a notebook cell
    /// splice must land inside the cell array the notebook actually has.
    /// Returning `Err`
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
            ProtocolNotification::NotebookOpen => {
                let params: DidOpenNotebookDocumentParams = decode_params(raw_params)?;
                self.notebooks.open(params)?;
            }
            ProtocolNotification::NotebookChange => {
                let params: DidChangeNotebookDocumentParams = decode_params(raw_params)?;
                self.notebooks.change(params)?;
            }
            ProtocolNotification::NotebookSave => {
                let params: DidSaveNotebookDocumentParams = decode_params(raw_params)?;
                // A notebook save carries no state beyond the notification
                // itself, exactly like `textDocument/didSave`. Saving a
                // notebook that was never opened still runs the hook: it
                // observes the same absence the peer left behind.
                if !self.notebooks.contains(&params.notebook_document.uri) {
                    debug!(
                        uri = ?params.notebook_document.uri,
                        "saving a notebook that is not synchronized"
                    );
                }
            }
            ProtocolNotification::NotebookClose => {
                let params: DidCloseNotebookDocumentParams = decode_params(raw_params)?;
                self.notebooks.close(params);
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
            // Notebook synchronization is its own capability in LSP, not a
            // mode of `textDocumentSync`, so the text-document switches below
            // do not gate it — its own opt-in does. A server that never called
            // `notebook_document_sync` advertises nothing, so a conformant
            // client sends no notebook notification; ignoring one that arrives
            // anyway keeps an unadvertised capability from mutating the
            // notebook layer, opening cell Documents against the connection's
            // budgets, or reaching a hook (ADR 0034).
            ProtocolNotification::NotebookOpen
            | ProtocolNotification::NotebookChange
            | ProtocolNotification::NotebookSave
            | ProtocolNotification::NotebookClose => self.notebook_sync,
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
    async fn initialize(&mut self, request: AdmittedRequest) -> Flow {
        // A second `initialize` after the transaction has run is invalid.
        if !matches!(self.lifecycle, Lifecycle::Uninitialized(_)) {
            self.failure_reporter.report(
                ConnectionFailureCategory::Protocol,
                Some(ConnectionDirection::Inbound),
                Some("initialize"),
                Some(request.id()),
            );
            request.respond(Err(LspError::ServerError {
                code: -32600,
                message: "server already initialized".into(),
                data: None,
            }));
            return Flow::Continue;
        }

        // Malformed `initialize` params leave the transaction unspent: the
        // client may retry with a valid request, so stay uninitialized.
        let params = match decode_params::<InitializeParams>(request.params()) {
            Ok(params) => params,
            Err(err) => {
                self.failure_reporter.report(
                    ConnectionFailureCategory::Protocol,
                    Some(ConnectionDirection::Inbound),
                    Some("initialize"),
                    Some(request.id()),
                );
                request.respond(Err(err));
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
                request.respond(Err(LspError::internal("initialization failed")));
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
        // Notebook sync carries its own capability and its own opt-in. The
        // generated catalog already holds whatever `notebook_document_sync`
        // contributed; the engine only records whether the built-ins it drives
        // are reachable at all on this connection (ADR 0034).
        self.notebook_sync = router.notebook_sync_enabled();
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
                    request.id().clone(),
                    request.span().clone(),
                    self.client.clone(),
                    established,
                )
                .with_work_done_token(work_done_token);
                match hook
                    .invoke((Arc::clone(&self.state), ctx, params, request.cancellation()))
                    .instrument(request.span().clone())
                    .await
                {
                    Ok(server_info) => server_info,
                    Err(err) => {
                        // ADR 0018: on_initialize failure sends that error, then
                        // enters the close path; the frozen Router and
                        // established Workspace are never exposed to later
                        // dispatch.
                        request.respond(Err(err));
                        return Flow::Close(CloseCause::InitializeFailed);
                    }
                }
            }
            None => None,
        };

        request.respond(encode_body(&WireInitializeResult {
            capabilities,
            server_info,
        }));
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

    /// Decode one user request and spawn it through the Service stack.
    ///
    /// The session races the call against cancellation and the deadline the
    /// Layer stack arms, and answers exactly once.
    fn spawn_service_request(&mut self, request: AdmittedRequest) {
        // Precedence guarantees the connection is running here.
        let Lifecycle::Running(service) = &self.lifecycle else {
            request.respond(Err(LspError::ServerNotInitialized));
            return;
        };
        let service = Arc::clone(service);
        let params = match decode_value(request.params()) {
            Ok(params) => params,
            Err(error) => {
                self.failure_reporter.report_unvalidated_inbound_method(
                    ConnectionFailureCategory::Protocol,
                    Some(request.id()),
                );
                request.respond(Err(error));
                return;
            }
        };
        let work_done_token = match request_token::<ProgressToken>(&params, "workDoneToken") {
            Ok(token) => token,
            Err(error) => {
                request.respond(Err(LspError::invalid_params(error)));
                return;
            }
        };
        let partial_result_token = if crate::partial_result::supports_method(request.method()) {
            match request_token::<ProgressToken>(&params, "partialResultToken") {
                Ok(token) => token,
                Err(error) => {
                    request.respond(Err(LspError::invalid_params(error)));
                    return;
                }
            }
        } else {
            None
        };
        let method = request.method().to_owned();
        let id = request.id().clone();
        let ctx = ServerContext::for_request(
            id.clone(),
            request.span().clone(),
            self.client.clone(),
            self.established_workspace(),
        )
        .with_cancellation(request.cancellation())
        .with_work_done_token(work_done_token)
        .with_partial_result(method.clone(), partial_result_token);
        let state = Arc::clone(&self.state);
        self.protocol
            .spawn_request(request, move |handler_timeout| async move {
                let call = IncomingCall::request(method, id, params, ctx, state, handler_timeout);
                // Cancellation and deadline drop this future before the
                // session answers, so the sink closes before the response in
                // every ending.
                let _partial_results = FinishOnDrop(call.context().partial_result_scope());
                service.call(call).await
            });
    }

    /// The engine's one close operation (ADR 0018), run once by the read loop.
    ///
    /// The session closes first: new outbound work is rejected, every pending
    /// `ClientHandle` request is resolved, every handler task is aborted and
    /// joined, and the writer drains. Documents and notebooks are Server
    /// endpoint state, so their lifecycle stays out of the shared session even
    /// though close releases retained snapshots.
    async fn close(&mut self) -> SessionEnd<CloseCause> {
        self.lifecycle = Lifecycle::Exited;
        let end = self.protocol.finish().await;
        self.documents.clear();
        self.notebooks.clear();
        end
    }
}

/// Ends a request's partial-result sink when the handler future is dropped.
struct FinishOnDrop(Option<PartialResultScope>);

impl Drop for FinishOnDrop {
    fn drop(&mut self) {
        if let Some(scope) = &self.0 {
            scope.finish();
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use gen_lsp_types::ProgressToken;
    use tracing_subscriber::layer::SubscriberExt;

    use tokio_util::sync::CancellationToken;

    use crate::raw::RawMessage;
    use crate::transport::{TransportError, TransportWriter};

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
    fn every_ending_maps_to_one_outcome_or_a_transport_error() {
        assert_eq!(
            into_result(SessionEnd::Endpoint(CloseCause::Exit { code: 0 })).unwrap(),
            Outcome::Exit { code: 0 }
        );
        assert_eq!(
            into_result(SessionEnd::ReaderEof).unwrap(),
            Outcome::TransportClosed
        );
        assert_eq!(
            into_result(SessionEnd::WriterFailed).unwrap(),
            Outcome::WriterFailed
        );
        assert_eq!(
            into_result(SessionEnd::Endpoint(CloseCause::InitializeFailed)).unwrap(),
            Outcome::InitializeFailed
        );
        assert!(matches!(
            into_result(SessionEnd::ReaderFailed(TransportError::Malformed(
                "bad".into()
            ))),
            Err(Error::Transport(_))
        ));
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
}
