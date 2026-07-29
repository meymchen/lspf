//! Connection-owned protocol engine for the 0.2 `Server<S>`.
//!
//! This slice serves a connection end to end for the lifecycle plus typed
//! custom requests, notifications, and commands. `initialize` is the one
//! bounded transaction that can conditionally extend the Router, freeze it,
//! generate capabilities, establish the connection's [`Workspace`],
//! [`Documents`], and negotiated position encoding, and run the
//! `on_initialize` lifecycle hook — all without exposing partial state
//! (ADR 0017, ADR 0018). `shutdown` and `exit` close the session. Inbound
//! requests reserve their IDs before user work is spawned; the engine's atomic
//! completion gate then arbitrates success, errors, and cancellation.

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
use crate::client::Client;
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

/// Drive a [`Server`] over `transport` until the peer exits, the transport
/// closes, a transport error ends the session, or a failed initialize
/// transaction enters the terminal close path.
///
/// The writer half moves into a send-loop task draining an unbounded channel;
/// the read-loop owns the reader and processes one envelope at a time.
pub(crate) async fn run<S, T>(server: Server<S>, transport: T) -> Result<()>
where
    S: Send + Sync + 'static,
    T: Transport,
{
    let (mut reader, writer) = transport.split();
    let (out_tx, out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let client = Client::new(out_tx.clone());
    let runtime = default_runtime();
    let send_handle = runtime.spawn(send_loop(writer, out_rx, client.clone()));
    let mut engine = ProtocolEngine::new(server, runtime, out_tx, client);

    let outcome = loop {
        engine.tasks.reap_finished().await;
        let msg = match reader.recv().await {
            Ok(msg) => msg,
            Err(TransportError::Closed) => {
                warn!("transport closed by peer before exit notification");
                engine.close().await;
                break Ok(());
            }
            Err(e) => {
                engine.close().await;
                break Err(Error::Transport(e));
            }
        };

        match engine.dispatch(msg).await {
            Flow::Continue => {}
            // `exit`, and the terminal close path a failed initialize enters,
            // both end the read-loop; the send-loop then drains what is queued.
            Flow::Exit | Flow::Close => {
                engine.close().await;
                break Ok(());
            }
        }
    };

    // Drop the master sender so the send-loop drains what is queued and exits.
    drop(engine);
    send_handle.join().await;
    outcome
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

#[derive(Clone, Default)]
struct InboundRegistry {
    entries: Arc<Mutex<HashMap<RequestId, Option<CancellationToken>>>>,
}

impl InboundRegistry {
    fn reserve(&self, id: RequestId, cancellation: Option<CancellationToken>) -> bool {
        let mut entries = self.entries.lock().unwrap();
        if entries.contains_key(&id) {
            return false;
        }
        entries.insert(id, cancellation);
        true
    }

    fn complete(
        &self,
        out_tx: &UnboundedSender<RawMessage>,
        id: RequestId,
        result: std::result::Result<Bytes, LspError>,
    ) {
        if self.entries.lock().unwrap().remove(&id).is_some() {
            enqueue_encoded(out_tx, id, result);
        }
    }

    fn complete_cancellation(&self, out_tx: &UnboundedSender<RawMessage>, id: &RequestId) {
        let token = {
            let mut entries = self.entries.lock().unwrap();
            if entries.get(id).is_some_and(Option::is_some) {
                entries.remove(id).flatten()
            } else {
                None
            }
        };
        if let Some(token) = token {
            token.cancel();
            enqueue_encoded(out_tx, id.clone(), Err(LspError::RequestCancelled));
        }
    }

    fn cancel_all(&self) {
        let entries = std::mem::take(&mut *self.entries.lock().unwrap());
        for cancellation in entries.into_values().flatten() {
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
            client.close_outbound();
            return;
        }
    }
    while let Some(msg) = out_rx.recv().await {
        if let Err(e) = writer.send(msg).await {
            warn!(error = %e, "send_loop: transport write failed while draining");
            return;
        }
    }
    if let Err(e) = writer.shutdown().await {
        warn!(error = %e, "send_loop: transport shutdown failed");
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
/// selection, request registration, cancellation, task ownership, and terminal
/// response arbitration all remain behind this boundary.
struct ProtocolEngine<S, R> {
    state: Arc<S>,
    documents: Documents,
    workspace: Option<Workspace>,
    lifecycle: Lifecycle<S>,
    inbound: InboundRegistry,
    tasks: TaskGroup<R>,
    out_tx: UnboundedSender<RawMessage>,
    client: Client,
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
        }
    }

    async fn dispatch(&mut self, msg: RawMessage) -> Flow {
        dispatch(
            &self.state,
            &self.documents,
            &mut self.workspace,
            &self.out_tx,
            &self.client,
            &mut self.lifecycle,
            &self.inbound,
            &mut self.tasks,
            msg,
        )
        .await
    }

    async fn close(&mut self) {
        if matches!(self.lifecycle, Lifecycle::Exited) {
            return;
        }
        self.lifecycle = Lifecycle::Exited;
        self.client.close_connection();
        self.inbound.cancel_all();
        self.tasks.abort_and_join().await;
        self.client.close_outbound();
    }
}

enum Flow {
    Continue,
    Exit,
    /// The terminal close path taken after a failed initialize transaction
    /// (ADR 0018): the fixed error is already enqueued; end the session rather
    /// than returning to an uninitialized state.
    Close,
}

#[allow(clippy::too_many_arguments)]
async fn dispatch<S>(
    state: &Arc<S>,
    documents: &Documents,
    workspace: &mut Option<Workspace>,
    out_tx: &UnboundedSender<RawMessage>,
    client: &Client,
    phase: &mut Lifecycle<S>,
    inbound: &InboundRegistry,
    tasks: &mut TaskGroup<impl Runtime>,
    msg: RawMessage,
) -> Flow
where
    S: Send + Sync + 'static,
{
    match msg {
        RawMessage::Request { id, method, params } => {
            let span = info_span!("request", method = %method, id = ?id);
            let cancellation = (method != "initialize").then(CancellationToken::new);
            if !inbound.reserve(id.clone(), cancellation.clone()) {
                enqueue_error(
                    out_tx,
                    id,
                    LspError::invalid_request("duplicate request id"),
                );
                return Flow::Continue;
            }

            // Initialize precedence: until `initialize` completes, refuse
            // every other request with `ServerNotInitialized`.
            if method != "initialize"
                && matches!(phase, Lifecycle::Uninitialized(_) | Lifecycle::Initializing)
            {
                inbound.complete(out_tx, id, Err(LspError::ServerNotInitialized));
                return Flow::Continue;
            }
            // After `shutdown`, every request is invalid until `exit`.
            if matches!(phase, Lifecycle::ShuttingDown | Lifecycle::Exited) {
                inbound.complete(
                    out_tx,
                    id,
                    Err(LspError::invalid_request("invalid request")),
                );
                return Flow::Continue;
            }

            match method.as_ref() {
                "initialize" => {
                    return initialize(
                        state, documents, workspace, out_tx, client, phase, inbound, &span, id,
                        params,
                    )
                    .await;
                }
                "shutdown" => {
                    inbound.complete(out_tx, id, encode_body(&serde_json::Value::Null));
                    *phase = Lifecycle::ShuttingDown;
                }
                _other => {
                    // Precedence guarantees the connection is running here.
                    let service = match phase {
                        Lifecycle::Running(service) => Arc::clone(service),
                        _ => {
                            inbound.complete(out_tx, id, Err(LspError::ServerNotInitialized));
                            return Flow::Continue;
                        }
                    };
                    let params = match decode_value(&params) {
                        Ok(params) => params,
                        Err(error) => {
                            inbound.complete(out_tx, id, Err(error));
                            return Flow::Continue;
                        }
                    };
                    spawn_service_request(
                        tasks,
                        Arc::clone(state),
                        service,
                        documents.clone(),
                        workspace.clone(),
                        out_tx.clone(),
                        client.clone(),
                        inbound.clone(),
                        span,
                        id,
                        method.into_owned(),
                        params,
                        cancellation.expect("non-initialize requests are cancellable"),
                    );
                }
            }
        }
        RawMessage::Notification { method, params } => match method.as_ref() {
            "exit" => return Flow::Exit,
            "$/cancelRequest" => {
                let bytes: &[u8] = if params.is_empty() { b"{}" } else { &params };
                match serde_json::from_slice::<CancelParams>(bytes) {
                    Ok(cancel) => inbound.complete_cancellation(out_tx, &cancel.id),
                    Err(error) => {
                        debug!(%error, "ignoring malformed $/cancelRequest");
                    }
                }
            }
            other => {
                // Until `initialize` completes, drop every notification but the
                // `exit` handled above. A registered custom notification then
                // dispatches with no response; an unregistered one is ignored.
                match phase {
                    Lifecycle::Running(service) => {
                        let params = match decode_value(&params) {
                            Ok(params) => params,
                            Err(error) => {
                                debug!(method = other, %error, "notification params ignored");
                                return Flow::Continue;
                            }
                        };
                        let span = info_span!("notification", method = %other);
                        let ctx = attach_workspace(
                            Context::for_notification(span, client.clone(), documents.clone()),
                            workspace,
                        );
                        let result = service
                            .call(IncomingCall::notification(
                                method.into_owned(),
                                params,
                                ctx,
                                Arc::clone(state),
                            ))
                            .await;
                        if !matches!(result, ServiceResult::NoResponse) {
                            warn!("notification service attempted to produce a response");
                        }
                    }
                    _ => debug!(method = other, "notification before running state ignored"),
                }
            }
        },
        RawMessage::Response { .. } => warn!("ignoring unexpected response"),
        RawMessage::ProtocolError { error } => {
            let _ = out_tx.send(RawMessage::ProtocolError { error });
        }
    }

    Flow::Continue
}

#[allow(clippy::too_many_arguments)]
fn spawn_service_request<S, R>(
    tasks: &mut TaskGroup<R>,
    state: Arc<S>,
    service: UserService<S>,
    documents: Documents,
    workspace: Option<Workspace>,
    out_tx: UnboundedSender<RawMessage>,
    client: Client,
    inbound: InboundRegistry,
    span: Span,
    id: RequestId,
    method: String,
    params: serde_json::Value,
    cancellation: CancellationToken,
) where
    S: Send + Sync + 'static,
    R: Runtime,
{
    let task_span = span.clone();
    tasks.spawn(async move {
        let ctx = attach_workspace(
            Context::for_request(id.clone(), task_span.clone(), client, documents),
            &workspace,
        )
        .with_cancellation(cancellation.clone());
        let call = IncomingCall::request(method, id.clone(), params, ctx, state);
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
        inbound.complete(&out_tx, id, result);
    });
}

/// Run the one `initialize` transaction (ADR 0017, ADR 0018).
///
/// In order: validate and consume the sole `initialize`; run
/// `configure_initialize` against a transactional registrar; on success commit
/// and permanently freeze the Router; establish the `Workspace`, `Documents`
/// encoding, and generated capabilities; run `on_initialize` for optional
/// `ServerInfo`; then enter the running state and reply. Any configuration,
/// validation, or `on_initialize` failure enqueues the fixed error and takes
/// the terminal close path rather than returning to uninitialized.
#[allow(clippy::too_many_arguments)]
async fn initialize<S>(
    state: &Arc<S>,
    documents: &Documents,
    workspace: &mut Option<Workspace>,
    out_tx: &UnboundedSender<RawMessage>,
    client: &Client,
    phase: &mut Lifecycle<S>,
    inbound: &InboundRegistry,
    span: &tracing::Span,
    id: RequestId,
    params: Bytes,
) -> Flow
where
    S: Send + Sync + 'static,
{
    // A second `initialize` after the transaction has run is invalid.
    if !matches!(phase, Lifecycle::Uninitialized(_)) {
        inbound.complete(
            out_tx,
            id,
            Err(LspError::ServerError {
                code: -32600,
                message: "server already initialized".into(),
                data: None,
            }),
        );
        return Flow::Continue;
    }

    // Malformed `initialize` params leave the transaction unspent: the client
    // may retry with a valid request, so stay uninitialized.
    let params = match decode_params::<InitializeParams>(&params) {
        Ok(params) => params,
        Err(err) => {
            inbound.complete(out_tx, id, Err(err));
            return Flow::Continue;
        }
    };

    // Take ownership of the pending registrations and callbacks; the transaction
    // consumes them exactly once.
    let pending = match std::mem::replace(phase, Lifecycle::Initializing) {
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

    // Run the conditional registration transaction against a registrar seeded
    // with all static registrations. A callback error or any combined-validation
    // conflict discards the whole transaction — the registrar (and every static
    // and conditional registration in it) is dropped, so nothing partial leaks.
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
            inbound.complete(out_tx, id, Err(LspError::internal("initialization failed")));
            return Flow::Close;
        }
    };

    // Commit: permanently freeze the Router before any capability is generated.
    let router = Arc::new(registrations.freeze());

    // Establish Workspace, Documents encoding, and generated capabilities from
    // InitializeParams before `on_initialize` observes them. Per ADR 0018's
    // precedence, the Workspace is established (step 4) before protocol-owned
    // fields are negotiated and capabilities generated (step 5).
    let established = Workspace::from_params(&params);
    *workspace = Some(established.clone());

    let position_encoding = documents.negotiate_position_encoding(&params);
    let mut capabilities = router.capabilities();
    capabilities.position_encoding = Some(position_encoding);

    // `on_initialize` may contribute optional ServerInfo but cannot register
    // routes or replace the generated capabilities.
    let server_info = match on_initialize {
        Some(hook) => {
            let ctx =
                Context::for_request(id.clone(), span.clone(), client.clone(), documents.clone())
                    .with_workspace(established);
            match hook(Arc::clone(state), ctx, params, CancellationToken::new())
                .instrument(span.clone())
                .await
            {
                Ok(server_info) => server_info,
                Err(err) => {
                    // ADR 0018: on_initialize failure sends that error, then
                    // enters the close path; the frozen Router and established
                    // Workspace are never exposed to later dispatch.
                    inbound.complete(out_tx, id, Err(err));
                    return Flow::Close;
                }
            }
        }
        None => None,
    };

    inbound.complete(
        out_tx,
        id,
        encode_body(&InitializeResult {
            capabilities,
            server_info,
        }),
    );
    *phase = Lifecycle::Running(build_service_stack(router, layers, concurrency_limit));
    Flow::Continue
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
