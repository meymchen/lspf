//! Client endpoint construction and connection driving.
//!
//! This module owns client-side initialization and reverse-handler policy. The
//! endpoint-neutral [`ProtocolSession`](crate::session::ProtocolSession) owns
//! correlation, admission, deadlines, task ownership, writer coordination,
//! and close.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use lsp_types::error_codes::SERVER_CANCELLED;
use lsp_types::notification::{Initialized, Notification};
use lsp_types::request::{Initialize, Request};
use lsp_types::{
    ClientCapabilities, ClientInfo, InitializeParams, InitializedParams, WorkDoneProgressParams,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug};

use crate::builder::SharedHandler;
use crate::client::ClientHandle;
use crate::codec::{decode_value, encode_body, erase_value};
use crate::error::{BuildError, LspError};
use crate::failure::FailureReporter;
use crate::raw::{RawMessage, RequestId};
use crate::resource_policy::ResourcePolicy;
use crate::runtime::{Runtime, TaskFuture, TaskSend, default_runtime, ensure_runtime_available};
use crate::service::{HandlerTimeout, ServiceResult};
use crate::session::{
    INBOUND_CAPACITY_EXHAUSTED, InboundReserveError, ProtocolSession, SessionInput,
    run_handler_with_deadline,
};
use crate::telemetry::{ConnectionTrace, Direction};
use crate::transport::{Transport, TransportError, TransportReader};
use crate::{ClientError, Error, Outcome, Result};

type ReverseFuture = Pin<Box<dyn TaskFuture<std::result::Result<Value, LspError>>>>;

#[cfg(not(target_arch = "wasm32"))]
type ReverseHandler = Arc<dyn Fn(Value, CancellationToken) -> ReverseFuture + Send + Sync>;

#[cfg(target_arch = "wasm32")]
type ReverseHandler = Arc<dyn Fn(Value, CancellationToken) -> ReverseFuture>;

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
    handlers: HashMap<&'static str, ReverseHandler>,
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
        let server = ServerHandle {
            inner: peer.clone(),
        };
        let mut engine = ClientEngine {
            handlers: self.handlers,
            protocol,
            peer,
            trace,
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
        server.notify::<Initialized>(InitializedParams {})?;

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
    handlers: HashMap<&'static str, ReverseHandler>,
    resource_policy: ResourcePolicy,
    error: Option<BuildError>,
}

impl ClientBuilder {
    fn new(capabilities: ClientCapabilities) -> Self {
        Self {
            capabilities,
            client_info: None,
            initialization_options: None,
            handlers: HashMap::new(),
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
    pub fn request<R, H, Fut>(mut self, handler: H) -> Self
    where
        R: Request,
        H: Fn(R::Params, CancellationToken) -> Fut
            + SharedHandler<(R::Params, CancellationToken), Fut>
            + 'static,
        Fut: Future<Output = std::result::Result<R::Result, LspError>> + TaskSend + 'static,
    {
        if is_reserved_reverse_method(R::METHOD) {
            self.record(BuildError::ReservedMethod(R::METHOD.to_string()));
            return self;
        }
        if self.handlers.contains_key(R::METHOD) {
            self.record(BuildError::DuplicateMethod(R::METHOD.to_string()));
            return self;
        }
        let handler = Arc::new(handler);
        self.handlers.insert(
            R::METHOD,
            Arc::new(move |params, cancellation| {
                let handler = Arc::clone(&handler);
                let params =
                    serde_json::from_value::<R::Params>(params).map_err(LspError::invalid_params);
                Box::pin(async move {
                    let params = params?;
                    let result = handler.invoke((params, cancellation)).await?;
                    erase_value(result)
                })
            }),
        );
        self
    }

    /// Validate the configuration and build a client over `transport` without
    /// performing I/O.
    pub fn build<T: Transport>(
        mut self,
        transport: T,
    ) -> std::result::Result<Client<T>, BuildError> {
        if let Err(error) = self.resource_policy.validate() {
            self.record(error);
        }
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(Client {
            transport,
            capabilities: self.capabilities,
            client_info: self.client_info,
            initialization_options: self.initialization_options,
            handlers: self.handlers,
            resource_policy: self.resource_policy,
        })
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

/// A cloneable typed handle for messages sent to the connected LSP server.
#[derive(Clone)]
pub struct ServerHandle {
    inner: ClientHandle,
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
        self.inner.notify::<N>(params)
    }

    /// Send one typed client-to-server request and await its correlated result.
    pub async fn request<R>(&self, params: R::Params) -> std::result::Result<R::Result, ClientError>
    where
        R: Request,
    {
        self.inner.request::<R>(params).await
    }
}

#[derive(Debug)]
enum ClientCloseCause {
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
            Self::ReaderEof => "reader_eof",
            Self::ReaderFailed(_) => "reader_failed",
            Self::WriterFailed => "writer_failed",
            Self::InitializeFailed => "initialize_failed",
        }
    }

    fn into_result(self) -> Result<Outcome> {
        match self {
            Self::ReaderEof => Ok(Outcome::TransportClosed),
            Self::ReaderFailed(error) => Err(Error::Transport(error)),
            Self::WriterFailed => Ok(Outcome::WriterFailed),
            Self::InitializeFailed => unreachable!("initialize failure returns from connect"),
        }
    }
}

struct ClientEngine<R> {
    handlers: HashMap<&'static str, ReverseHandler>,
    protocol: ProtocolSession<R, ClientHandle, ClientCloseCause>,
    peer: ClientHandle,
    trace: ConnectionTrace,
}

impl<R: Runtime> ClientEngine<R> {
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
            RawMessage::Notification { method, .. } => {
                debug!(%method, "unregistered server notification ignored");
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
                        code: SERVER_CANCELLED as i32,
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
        let Some(handler) = self.handlers.get(method.as_str()).cloned() else {
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
        self.protocol.spawn(
            async move {
                let handler_cancellation = cancellation.clone();
                let result = run_handler_with_deadline(
                    async move {
                        match handler(params, handler_cancellation).await {
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
                completion.complete(reservation, result);
            }
            .instrument(span),
            permit,
        );
    }
}

fn is_reserved_reverse_method(method: &str) -> bool {
    matches!(
        method,
        "initialize" | "initialized" | "shutdown" | "exit" | "$/cancelRequest"
    )
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
        workspace_folders: None,
        client_info,
        locale: None,
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}
