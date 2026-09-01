//! Client endpoint construction and connection driving.
//!
//! This module owns client-side initialization and reverse-handler policy. The
//! endpoint-neutral [`ProtocolSession`](crate::session::ProtocolSession) owns
//! correlation, admission, deadlines, task ownership, writer coordination,
//! and close.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use gen_lsp_types::{
    ClientCapabilities, ClientInfo, ExitNotification as Exit, InitializeParams,
    InitializeRequest as Initialize, InitializedNotification as Initialized, InitializedParams,
    LspErrorCodes, ProgressNotification as Progress, ProgressParams, ShutdownRequest as Shutdown,
    WorkDoneProgressCreateParams, WorkDoneProgressCreateRequest as WorkDoneProgressCreate,
    WorkDoneProgressParams,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, debug};

use crate::builder::SharedHandler;
use crate::client::ClientHandle;
use crate::client_progress::{ClientProgressRegistry, CreateOutcome};
use crate::codec::{decode_value, encode_body, encode_params, erase_value};
use crate::error::{BuildError, LspError};
use crate::failure::FailureReporter;
use crate::raw::{RawMessage, RequestId};
use crate::resource_policy::ResourcePolicy;
use crate::runtime::{Runtime, TaskFuture, TaskSend, default_runtime, ensure_runtime_available};
use crate::service::{HandlerTimeout, ServiceResult};
use crate::session::{
    INBOUND_CAPACITY_EXHAUSTED, InboundReserveError, ProtocolControl, ProtocolSession,
    SessionInput, run_handler_with_deadline,
};
use crate::telemetry::{ConnectionTrace, Direction};
use crate::transport::{Transport, TransportError, TransportReader};
use crate::types::notification::Notification;
use crate::types::request::Request;
use crate::{ClientError, Error, Outcome, Result};

type ReverseFuture = Pin<Box<dyn TaskFuture<std::result::Result<Value, LspError>>>>;
type ReverseNotificationFuture = Pin<Box<dyn TaskFuture<std::result::Result<(), LspError>>>>;

#[cfg(not(target_arch = "wasm32"))]
type ReverseHandler =
    Arc<dyn Fn(ClientContext, Value, CancellationToken) -> ReverseFuture + Send + Sync>;

#[cfg(not(target_arch = "wasm32"))]
type ReverseNotificationHandler =
    Arc<dyn Fn(ClientContext, Value) -> ReverseNotificationFuture + Send + Sync>;

#[cfg(target_arch = "wasm32")]
type ReverseHandler = Arc<dyn Fn(ClientContext, Value, CancellationToken) -> ReverseFuture>;

#[cfg(target_arch = "wasm32")]
type ReverseNotificationHandler = Arc<dyn Fn(ClientContext, Value) -> ReverseNotificationFuture>;

type ConnectionFuture = Pin<Box<dyn TaskFuture<Result<Outcome>>>>;

/// An LSP client endpoint configured for one connection.
///
/// Construct it with [`Client::builder`], supplying the caller-provided
/// [`Transport`] to [`ClientBuilder::build`].
pub struct Client<T = ()> {
    transport: T,
    capabilities: ClientCapabilities,
    client_info: Option<ClientInfo>,
    initialization_options: Option<Value>,
    request_handlers: HashMap<&'static str, ReverseHandler>,
    notification_handlers: HashMap<&'static str, ReverseNotificationHandler>,
    resource_policy: ResourcePolicy,
}

impl Client<()> {
    /// Begin configuring a client with the capabilities sent in `initialize`.
    pub fn builder(capabilities: ClientCapabilities) -> ClientBuilder {
        ClientBuilder::new(capabilities)
    }
}

impl<T: Transport> Client<T> {
    /// Initialize this client over its caller-provided Transport.
    ///
    /// The returned connection has completed the `initialize` request and has
    /// enqueued the required `initialized` notification. Drive it with
    /// [`ClientConnection::serve`] while using its [`ServerHandle`] for typed
    /// client-to-server calls.
    pub async fn connect(self) -> Result<ClientConnection> {
        ensure_runtime_available()?;
        let trace = ConnectionTrace::new();
        let failure_reporter = FailureReporter::new(None, trace.id());
        let span = trace.span();
        let (mut reader, writer) = self.transport.split();
        let (protocol, peer) = ProtocolSession::start(
            default_runtime(),
            self.resource_policy,
            writer,
            trace,
            span.clone(),
            failure_reporter.clone(),
            ClientCloseCause::writer_failed,
            ClientHandle::new,
        );
        let lifecycle = Arc::new(ClientLifecycle {
            phase: Mutex::new(ClientPhase::Initializing),
            protocol: protocol.control(),
        });
        let progress = ClientProgressRegistry::default();
        let server = ServerHandle {
            inner: peer.clone(),
            lifecycle: Arc::clone(&lifecycle),
            progress: progress.clone(),
        };
        let mut engine = ClientEngine {
            request_handlers: self.request_handlers,
            notification_handlers: self.notification_handlers,
            progress,
            protocol,
            peer,
            trace,
            lifecycle,
        };

        let params = initialize_params(
            self.capabilities,
            self.client_info,
            self.initialization_options,
        );
        if let Err(error) = engine.initialize(&mut reader, params).await {
            engine
                .protocol
                .request_close(ClientCloseCause::InitializeFailed);
            engine.protocol.close().await;
            trace.connection_closed("initialize_failed");
            return Err(error);
        }
        if let Err(error) = server
            .inner
            .notify_required::<Initialized>(InitializedParams {})
        {
            server
                .lifecycle
                .protocol
                .request_close(ClientCloseCause::InitializeFailed);
            engine.protocol.close().await;
            trace.connection_closed("initialize_failed");
            return Err(error.into());
        }
        server.lifecycle.mark_running();

        Ok(ClientConnection {
            server,
            driver: Box::pin(engine.serve(reader).instrument(span)),
        })
    }
}

/// Builder for one [`Client`] connection.
pub struct ClientBuilder {
    capabilities: ClientCapabilities,
    client_info: Option<ClientInfo>,
    initialization_options: Option<Value>,
    request_handlers: HashMap<&'static str, ReverseHandler>,
    notification_handlers: HashMap<&'static str, ReverseNotificationHandler>,
    resource_policy: ResourcePolicy,
    error: Option<BuildError>,
}

impl ClientBuilder {
    fn new(capabilities: ClientCapabilities) -> Self {
        Self {
            capabilities,
            client_info: None,
            initialization_options: None,
            request_handlers: HashMap::new(),
            notification_handlers: HashMap::new(),
            resource_policy: ResourcePolicy::default(),
            error: None,
        }
    }

    /// Set the client name and optional version sent in `initialize`.
    pub fn client_info(mut self, client_info: ClientInfo) -> Self {
        self.client_info = Some(client_info);
        self
    }

    /// Set the caller-owned initialization options sent verbatim in `initialize`.
    pub fn initialization_options(mut self, options: Value) -> Self {
        self.initialization_options = Some(options);
        self
    }

    /// Replace the finite budgets and deadlines owned by this connection.
    pub fn resource_policy(mut self, policy: ResourcePolicy) -> Self {
        self.resource_policy = policy;
        self
    }

    /// Register one typed server-to-client request handler.
    ///
    /// The handler receives a [`ClientContext`], decoded parameters, and a
    /// request-scoped [`CancellationToken`]. `window/workDoneProgress/create`
    /// reserves its token in this connection's progress registry and commits
    /// it only when the request completes successfully.
    pub fn request<R, H, Fut>(mut self, handler: H) -> Self
    where
        R: Request,
        H: Fn(ClientContext, R::Params, CancellationToken) -> Fut
            + SharedHandler<(ClientContext, R::Params, CancellationToken), Fut>
            + 'static,
        Fut: Future<Output = std::result::Result<R::Result, LspError>> + TaskSend + 'static,
    {
        if is_reserved_reverse_method(R::METHOD) {
            self.record(BuildError::ReservedMethod(R::METHOD.to_string()));
            return self;
        }
        if self.request_handlers.contains_key(R::METHOD) {
            self.record(BuildError::DuplicateMethod(R::METHOD.to_string()));
            return self;
        }
        let handler = Arc::new(handler);
        self.request_handlers.insert(
            R::METHOD,
            Arc::new(move |ctx, params, cancellation| {
                let handler = Arc::clone(&handler);
                let params =
                    serde_json::from_value::<R::Params>(params).map_err(LspError::invalid_params);
                Box::pin(async move {
                    let params = params?;
                    let result = handler.invoke((ctx, params, cancellation)).await?;
                    erase_value(result)
                })
            }),
        );
        self
    }

    /// Register one typed server-to-client notification handler.
    ///
    /// The handler receives a connection-scoped [`ClientContext`] and decoded
    /// parameters. Notifications have no response or cancellation token.
    /// Registered `$/progress` notifications are dispatched only for a token
    /// created on this connection and a valid begin, report, or end transition;
    /// the first end removes the token, so duplicate and late progress is
    /// ignored.
    pub fn notification<N, H, Fut>(mut self, handler: H) -> Self
    where
        N: Notification,
        H: Fn(ClientContext, N::Params) -> Fut
            + SharedHandler<(ClientContext, N::Params), Fut>
            + 'static,
        Fut: Future<Output = ()> + TaskSend + 'static,
    {
        if is_reserved_reverse_method(N::METHOD) {
            self.record(BuildError::ReservedMethod(N::METHOD.to_string()));
            return self;
        }
        if self.notification_handlers.contains_key(N::METHOD) {
            self.record(BuildError::DuplicateMethod(N::METHOD.to_string()));
            return self;
        }
        let handler = Arc::new(handler);
        self.notification_handlers.insert(
            N::METHOD,
            Arc::new(move |ctx, params| {
                let handler = Arc::clone(&handler);
                let params =
                    serde_json::from_value::<N::Params>(params).map_err(LspError::invalid_params);
                Box::pin(async move {
                    handler.invoke((ctx, params?)).await;
                    Ok(())
                })
            }),
        );
        self
    }

    /// Validate the configuration and build a client over `transport` without
    /// performing I/O.
    pub fn build<T: Transport>(self, transport: T) -> std::result::Result<Client<T>, BuildError> {
        let validated = self.validate()?;
        Ok(Client {
            transport,
            capabilities: validated.capabilities,
            client_info: validated.client_info,
            initialization_options: validated.initialization_options,
            request_handlers: validated.request_handlers,
            notification_handlers: validated.notification_handlers,
            resource_policy: validated.resource_policy,
        })
    }

    pub(crate) fn validate(mut self) -> std::result::Result<Self, BuildError> {
        if let Err(error) = self.resource_policy.validate() {
            self.record(error);
        }
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        Ok(self)
    }

    fn record(&mut self, error: BuildError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }
}

/// A connected client endpoint and its remaining inbound driver.
pub struct ClientConnection {
    server: ServerHandle,
    driver: ConnectionFuture,
}

impl ClientConnection {
    /// Clone the typed handle used to send messages to the connected server.
    pub fn server(&self) -> ServerHandle {
        self.server.clone()
    }

    /// Drive reverse traffic until the Transport closes or fails.
    pub async fn serve(self) -> Result<Outcome> {
        self.driver.await
    }
}

/// Per-call handle passed to server-to-client handlers.
///
/// It exposes only protocol-facing connection state: the current request ID
/// (if any), its tracing span, and a cloneable [`ServerHandle`] for typed
/// client-to-server calls. Editor workspace, UI, filesystem, and extension
/// host policy remain owned by the application.
#[derive(Clone, Debug)]
pub struct ClientContext {
    request_id: Option<RequestId>,
    span: Span,
    server: ServerHandle,
}

impl ClientContext {
    fn for_request(request_id: RequestId, span: Span, server: ServerHandle) -> Self {
        Self {
            request_id: Some(request_id),
            span,
            server,
        }
    }

    fn for_notification(span: Span, server: ServerHandle) -> Self {
        Self {
            request_id: None,
            span,
            server,
        }
    }

    /// The JSON-RPC request ID, or `None` while handling a notification.
    pub fn request_id(&self) -> Option<&RequestId> {
        self.request_id.as_ref()
    }

    /// The tracing span associated with the incoming call.
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// A cheap clone of the typed handle for this connection's LSP server.
    pub fn server(&self) -> ServerHandle {
        self.server.clone()
    }
}

/// A cloneable typed handle for messages sent to the connected LSP server.
#[derive(Clone)]
pub struct ServerHandle {
    inner: ClientHandle,
    lifecycle: Arc<ClientLifecycle>,
    progress: ClientProgressRegistry,
}

struct RequestProgressGuard {
    registry: ClientProgressRegistry,
    token: gen_lsp_types::ProgressToken,
}

impl Drop for RequestProgressGuard {
    fn drop(&mut self) {
        self.registry.remove(&self.token);
    }
}

impl std::fmt::Debug for ServerHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerHandle")
            .finish_non_exhaustive()
    }
}

impl ServerHandle {
    /// Encode and enqueue one typed client-to-server notification.
    pub fn notify<N>(&self, params: N::Params) -> std::result::Result<(), ClientError>
    where
        N: Notification,
    {
        self.lifecycle.ensure_running(N::METHOD)?;
        self.inner.notify::<N>(params)
    }

    /// Send one typed client-to-server request and await its correlated result.
    ///
    /// A top-level `workDoneToken` in the encoded LSP params is registered for
    /// this request's lifetime, so matching `$/progress` notifications use the
    /// same ordered, connection-local lifecycle as server-created progress.
    pub async fn request<R>(&self, params: R::Params) -> std::result::Result<R::Result, ClientError>
    where
        R: Request,
    {
        self.lifecycle.ensure_running(R::METHOD)?;
        let params = encode_params(&params).map_err(ClientError::Serialize)?;
        let token = work_done_token(&params)?;
        let _request_progress_guard = match token {
            Some(token) => {
                if !self.progress.try_register_request(token.clone()) {
                    return Err(ClientError::InvalidHelperParams(
                        "duplicate work-done progress token".to_string(),
                    ));
                }
                Some(RequestProgressGuard {
                    registry: self.progress.clone(),
                    token,
                })
            }
            None => None,
        };
        self.inner.request_encoded::<R>(params).await
    }

    /// Request a graceful LSP shutdown from the connected server.
    ///
    /// Success cancels every other pending request and reverse request. Only
    /// [`Self::exit`] or [`Self::disconnect`] remains valid afterwards.
    pub async fn shutdown(&self) -> std::result::Result<(), ClientError> {
        self.lifecycle.begin_shutdown()?;
        let result = self.inner.request::<Shutdown>(()).await;
        self.lifecycle.finish_shutdown(&result);
        result
    }

    /// Send the required LSP `exit` notification after successful shutdown.
    pub fn exit(&self) -> std::result::Result<(), ClientError> {
        self.lifecycle.begin_exit()?;
        let result = self.inner.notify_required::<Exit>(());
        self.lifecycle
            .protocol
            .request_close(ClientCloseCause::Exit);
        result
    }

    /// End the local connection without sending shutdown or exit traffic.
    ///
    /// Disconnect is idempotent and resolves pending work through the shared
    /// session close path.
    pub fn disconnect(&self) {
        if self.lifecycle.disconnect() {
            self.lifecycle
                .protocol
                .request_close(ClientCloseCause::Disconnect);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientPhase {
    Initializing,
    Running,
    ShutdownPending,
    ShuttingDown,
    Exited,
    Disconnected,
}

#[derive(Clone, Copy)]
enum ClientTransition {
    Initialized,
    BeginShutdown,
    ShutdownSucceeded,
    ShutdownFailed,
    Exit,
    Disconnect,
}

#[derive(Clone, Copy)]
enum ClientWork {
    Outbound,
    Reverse,
}

impl ClientPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Initializing => "initializing",
            Self::Running => "running",
            Self::ShutdownPending => "shutdown is pending",
            Self::ShuttingDown => "shut down",
            Self::Exited => "exited",
            Self::Disconnected => "disconnected",
        }
    }

    fn transition(self, transition: ClientTransition) -> Option<Self> {
        match (self, transition) {
            (Self::Initializing, ClientTransition::Initialized) => Some(Self::Running),
            (Self::Running, ClientTransition::BeginShutdown) => Some(Self::ShutdownPending),
            (Self::ShutdownPending, ClientTransition::ShutdownSucceeded) => {
                Some(Self::ShuttingDown)
            }
            (Self::ShutdownPending, ClientTransition::ShutdownFailed) => Some(Self::Running),
            (Self::ShuttingDown, ClientTransition::Exit) => Some(Self::Exited),
            (Self::Exited | Self::Disconnected, ClientTransition::Disconnect) => None,
            (_, ClientTransition::Disconnect) => Some(Self::Disconnected),
            _ => None,
        }
    }

    fn permits(self, work: ClientWork) -> bool {
        match work {
            ClientWork::Outbound => self == Self::Running,
            ClientWork::Reverse => matches!(self, Self::Running | Self::ShutdownPending),
        }
    }
}

#[cfg(feature = "fuzzing")]
pub(crate) fn fuzz_lifecycle_sequence(data: &[u8]) {
    let mut phase = ClientPhase::Initializing;

    for byte in data {
        let before = phase;
        let transition = match byte % 6 {
            0 => ClientTransition::Initialized,
            1 => ClientTransition::BeginShutdown,
            2 => ClientTransition::ShutdownSucceeded,
            3 => ClientTransition::ShutdownFailed,
            4 => ClientTransition::Exit,
            _ => ClientTransition::Disconnect,
        };

        if let Some(next) = phase.transition(transition) {
            phase = next;
        }

        // Work admission is part of the lifecycle contract and must remain a
        // total operation in every state reached by arbitrary peer sequences.
        let _ = phase.permits(ClientWork::Outbound);
        let _ = phase.permits(ClientWork::Reverse);

        if matches!(before, ClientPhase::Exited | ClientPhase::Disconnected) {
            assert_eq!(phase, before, "terminal lifecycle state changed");
        }
    }
}

struct ClientLifecycle {
    phase: Mutex<ClientPhase>,
    protocol: ProtocolControl<ClientHandle, ClientCloseCause>,
}

impl ClientLifecycle {
    fn mark_running(&self) {
        self.transition("initialize", ClientTransition::Initialized)
            .expect("initialization completes only from the initializing phase");
    }

    fn ensure_running(&self, operation: &'static str) -> std::result::Result<(), ClientError> {
        let phase = *self.phase.lock().unwrap();
        if phase.permits(ClientWork::Outbound) && !is_reserved_reverse_method(operation) {
            Ok(())
        } else {
            Err(Self::invalid(operation, phase))
        }
    }

    fn begin_shutdown(&self) -> std::result::Result<(), ClientError> {
        self.transition("shutdown", ClientTransition::BeginShutdown)
    }

    fn finish_shutdown(&self, result: &std::result::Result<(), ClientError>) {
        let transition = if result.is_ok() {
            ClientTransition::ShutdownSucceeded
        } else {
            ClientTransition::ShutdownFailed
        };
        if self.transition("shutdown", transition).is_ok() && result.is_ok() {
            self.protocol.successful_shutdown();
        }
    }

    fn begin_exit(&self) -> std::result::Result<(), ClientError> {
        self.transition("exit", ClientTransition::Exit)
    }

    fn disconnect(&self) -> bool {
        self.transition("disconnect", ClientTransition::Disconnect)
            .is_ok()
    }

    fn rejects_reverse_work(&self) -> bool {
        !self.phase.lock().unwrap().permits(ClientWork::Reverse)
    }

    fn transition(
        &self,
        operation: &'static str,
        transition: ClientTransition,
    ) -> std::result::Result<(), ClientError> {
        let mut phase = self.phase.lock().unwrap();
        let current = *phase;
        if let Some(next) = current.transition(transition) {
            *phase = next;
            Ok(())
        } else {
            Err(Self::invalid(operation, current))
        }
    }

    fn invalid(operation: &'static str, phase: ClientPhase) -> ClientError {
        ClientError::InvalidLifecycle {
            operation,
            state: phase.as_str(),
        }
    }
}

#[derive(Debug)]
enum ClientCloseCause {
    Exit,
    Disconnect,
    ReaderEof,
    ReaderFailed(TransportError),
    WriterFailed,
    InitializeFailed,
}

impl ClientCloseCause {
    fn writer_failed() -> Self {
        Self::WriterFailed
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Disconnect => "disconnect",
            Self::ReaderEof => "reader_eof",
            Self::ReaderFailed(_) => "reader_failed",
            Self::WriterFailed => "writer_failed",
            Self::InitializeFailed => "initialize_failed",
        }
    }

    fn into_result(self) -> Result<Outcome> {
        match self {
            Self::Exit => Ok(Outcome::Exit { code: 0 }),
            Self::Disconnect => Ok(Outcome::TransportClosed),
            Self::ReaderEof => Ok(Outcome::TransportClosed),
            Self::ReaderFailed(error) => Err(Error::Transport(error)),
            Self::WriterFailed => Ok(Outcome::WriterFailed),
            Self::InitializeFailed => unreachable!("initialize failure returns from connect"),
        }
    }
}

struct ClientEngine<R> {
    request_handlers: HashMap<&'static str, ReverseHandler>,
    notification_handlers: HashMap<&'static str, ReverseNotificationHandler>,
    progress: ClientProgressRegistry,
    protocol: ProtocolSession<R, ClientHandle, ClientCloseCause>,
    peer: ClientHandle,
    trace: ConnectionTrace,
    lifecycle: Arc<ClientLifecycle>,
}

impl<R: Runtime> ClientEngine<R> {
    fn server_handle(&self) -> ServerHandle {
        ServerHandle {
            inner: self.peer.clone(),
            lifecycle: Arc::clone(&self.lifecycle),
            progress: self.progress.clone(),
        }
    }

    async fn initialize<Rd>(&mut self, reader: &mut Rd, params: InitializeParams) -> Result<()>
    where
        Rd: TransportReader,
    {
        let peer = self.peer.clone();
        let mut pending = Box::pin(peer.request::<Initialize>(params));
        loop {
            match futures_util::future::select(pending, Box::pin(reader.recv())).await {
                futures_util::future::Either::Left((result, _)) => {
                    result?;
                    return Ok(());
                }
                futures_util::future::Either::Right((message, still_pending)) => {
                    pending = still_pending;
                    match message {
                        Ok(message) => {
                            self.trace.message(Direction::Inbound, &message);
                            self.dispatch_during_initialize(message);
                        }
                        Err(error) => return Err(Error::Transport(error)),
                    }
                }
            }
        }
    }

    fn dispatch_during_initialize(&self, message: RawMessage) {
        match message {
            RawMessage::Response { id, result } => self.complete_response(id, result),
            RawMessage::Request { id, method, .. } if method == "initialize" => self
                .protocol
                .reject_inbound(id, LspError::invalid_request("duplicate initialize")),
            RawMessage::Request { id, .. } => self
                .protocol
                .reject_inbound(id, LspError::ServerNotInitialized),
            RawMessage::Notification { method, .. } => {
                debug!(%method, "notification during client initialization ignored");
            }
            RawMessage::ProtocolError { error } => self.protocol.send_protocol_error(error),
        }
    }

    async fn serve<Rd>(mut self, mut reader: Rd) -> Result<Outcome>
    where
        Rd: TransportReader,
    {
        loop {
            let message = match self.protocol.next_input(&mut reader).await {
                SessionInput::CloseRequested => break,
                SessionInput::OutboundFailed => {
                    self.protocol.request_close(ClientCloseCause::WriterFailed);
                    break;
                }
                SessionInput::Message(message) => message,
            };
            match message {
                Ok(message) => {
                    self.trace.message(Direction::Inbound, &message);
                    self.dispatch(message);
                }
                Err(TransportError::Closed) => {
                    self.protocol.request_close(ClientCloseCause::ReaderEof);
                    break;
                }
                Err(error) => {
                    self.protocol
                        .request_close(ClientCloseCause::ReaderFailed(error));
                    break;
                }
            }
        }
        self.lifecycle.disconnect();
        self.protocol.close().await;
        let cause = self.protocol.final_close_cause();
        self.trace.connection_closed(cause.as_str());
        cause.into_result()
    }

    fn dispatch(&mut self, message: RawMessage) {
        match message {
            RawMessage::Response { id, result } => self.complete_response(id, result),
            RawMessage::Request { id, method, params } => {
                self.dispatch_request(id, method.into_owned(), params)
            }
            RawMessage::Notification { method, params } if method == "$/cancelRequest" => {
                #[derive(serde::Deserialize)]
                struct CancelParams {
                    id: RequestId,
                }
                match crate::codec::decode_params::<CancelParams>(&params) {
                    Ok(params) => {
                        if let Some(reservation) = self.protocol.cancel_inbound(&params.id) {
                            self.protocol.complete_cancelled(reservation);
                        }
                    }
                    Err(error) => debug!(%error, "malformed cancellation ignored"),
                }
            }
            RawMessage::Notification { method, params } => {
                self.dispatch_notification(method.into_owned(), params)
            }
            RawMessage::ProtocolError { error } => self.protocol.send_protocol_error(error),
        }
    }

    fn complete_response(
        &self,
        id: RequestId,
        result: std::result::Result<bytes::Bytes, crate::JsonRpcError>,
    ) {
        let delivered = match id {
            RequestId::Number(number) if number > 0 => self
                .peer
                .outbound_registry()
                .complete(number as u32, result),
            _ => false,
        };
        if !delivered {
            debug!("ignoring response with unknown or non-numeric id");
        }
    }

    fn dispatch_request(&mut self, id: RequestId, method: String, params: bytes::Bytes) {
        let reserved = match self.protocol.reserve_inbound(id.clone(), &method, true) {
            Ok(reserved) => reserved,
            Err(InboundReserveError::DuplicateId) => {
                self.protocol
                    .reject_inbound(id, LspError::invalid_request("duplicate request id"));
                return;
            }
            Err(InboundReserveError::CapacityExhausted) => {
                self.protocol.reject_inbound(
                    id,
                    LspError::ServerError {
                        code: LspErrorCodes::ServerCancelled.into(),
                        message: INBOUND_CAPACITY_EXHAUSTED.to_string(),
                        data: None,
                    },
                );
                return;
            }
        };
        let reservation = reserved.reservation;
        let cancellation = reserved
            .cancellation
            .expect("reverse requests are cancellable");
        if self.lifecycle.rejects_reverse_work() {
            self.protocol.complete_inbound(
                reservation,
                Err(LspError::invalid_request("invalid request after shutdown")),
            );
            return;
        }
        if method == "initialize" {
            self.protocol.complete_inbound(
                reservation,
                Err(LspError::invalid_request("duplicate initialize")),
            );
            return;
        }
        let Some(handler) = self.request_handlers.get(method.as_str()).cloned() else {
            self.protocol.complete_inbound(
                reservation,
                Err(LspError::MethodNotFound(method.to_string())),
            );
            return;
        };
        let params = match decode_value(&params) {
            Ok(params) => params,
            Err(error) => {
                self.protocol.complete_inbound(reservation, Err(error));
                return;
            }
        };
        let progress_token = if method == WorkDoneProgressCreate::METHOD {
            let create =
                match serde_json::from_value::<WorkDoneProgressCreateParams>(params.clone()) {
                    Ok(create) => create,
                    Err(error) => {
                        self.protocol
                            .complete_inbound(reservation, Err(LspError::invalid_params(error)));
                        return;
                    }
                };
            if !self.progress.try_reserve_create(create.token.clone()) {
                self.protocol.complete_inbound(
                    reservation,
                    Err(LspError::invalid_params(
                        "duplicate work-done progress token",
                    )),
                );
                return;
            }
            Some(create.token)
        } else {
            None
        };
        let completion = self.protocol.completion_gate();
        let permit = Arc::clone(&reservation._permit);
        let timeout = HandlerTimeout::new(
            self.protocol.handler_timeout(),
            self.trace,
            method.clone(),
            reservation.id.clone(),
        );
        timeout.arm();
        let span = self.trace.request_span(&method, &reservation.id);
        let ctx =
            ClientContext::for_request(reservation.id.clone(), span.clone(), self.server_handle());
        let progress = self.progress.clone();
        self.protocol.spawn(
            async move {
                let handler_cancellation = cancellation.clone();
                let result = run_handler_with_deadline(
                    async move {
                        match handler(ctx, params, handler_cancellation).await {
                            Ok(value) => ServiceResult::Response(value),
                            Err(error) => ServiceResult::Error(error),
                        }
                    },
                    cancellation,
                    timeout,
                )
                .await;
                let result = match result {
                    ServiceResult::Response(value) => encode_body(&value),
                    ServiceResult::Error(error) => Err(error),
                    ServiceResult::NoResponse => {
                        Err(LspError::internal("reverse request returned no response"))
                    }
                };
                if let Some(token) = progress_token {
                    let outcome = if result.is_ok() {
                        CreateOutcome::Succeeded
                    } else {
                        CreateOutcome::Failed
                    };
                    let claimed = completion.try_complete_with(reservation, result, {
                        let progress = progress.clone();
                        let token = token.clone();
                        move || progress.finish_create(&token, outcome)
                    });
                    if !claimed {
                        progress.finish_create(&token, CreateOutcome::Failed);
                    }
                } else {
                    completion.complete(reservation, result);
                }
            }
            .instrument(span),
            permit,
        );
    }

    fn dispatch_notification(&mut self, method: String, params: bytes::Bytes) {
        if self.lifecycle.rejects_reverse_work() {
            debug!(%method, "notification after client shutdown ignored");
            return;
        }
        let params = match decode_value(&params) {
            Ok(params) => params,
            Err(error) => {
                debug!(%method, %error, "server notification with malformed params ignored");
                return;
            }
        };
        let progress_delivery = if method == Progress::METHOD {
            let progress = match serde_json::from_value::<ProgressParams>(params.clone()) {
                Ok(progress) => progress,
                Err(error) => {
                    debug!(%method, %error, "server progress with malformed params ignored");
                    return;
                }
            };
            match self.progress.accept(&progress) {
                Some(delivery) => Some(delivery),
                None => {
                    debug!(token = ?progress.token, "inactive or invalid server progress ignored");
                    return;
                }
            }
        } else {
            None
        };
        let Some(handler) = self.notification_handlers.get(method.as_str()).cloned() else {
            debug!(%method, "unregistered server notification ignored");
            return;
        };
        let span = self.trace.notification_span(&method);
        let ctx = ClientContext::for_notification(span.clone(), self.server_handle());
        self.protocol.spawn_notification(
            async move {
                let _progress_order = match progress_delivery {
                    Some(delivery) => Some(delivery.wait().await),
                    None => None,
                };
                if let Err(error) = handler(ctx, params).await {
                    debug!(%method, %error, "server notification with malformed params ignored");
                }
            }
            .instrument(span),
        );
    }
}

fn is_reserved_reverse_method(method: &str) -> bool {
    matches!(
        method,
        "initialize" | "initialized" | "shutdown" | "exit" | "$/cancelRequest"
    )
}

fn work_done_token(
    params: &[u8],
) -> std::result::Result<Option<gen_lsp_types::ProgressToken>, ClientError> {
    if params.is_empty() {
        return Ok(None);
    }
    let value = serde_json::from_slice::<Value>(params).map_err(ClientError::Serialize)?;
    let Some(token) = value.get("workDoneToken") else {
        return Ok(None);
    };
    if token.is_null() {
        return Ok(None);
    }
    serde_json::from_value(token.clone())
        .map(Some)
        .map_err(ClientError::Serialize)
}

#[allow(deprecated)]
fn initialize_params(
    capabilities: ClientCapabilities,
    client_info: Option<ClientInfo>,
    initialization_options: Option<Value>,
) -> InitializeParams {
    InitializeParams {
        process_id: None,
        root_path: None,
        root_uri: None,
        initialization_options,
        capabilities,
        trace: None,
        workspace_folders_initialize_params: Default::default(),
        client_info,
        locale: None,
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}
