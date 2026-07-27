//! Minimal protocol engine for the 0.2 `Server<S>` (PRD 0.2 slices 3–6).
//!
//! This slice serves a connection end to end for the lifecycle plus typed
//! custom requests, notifications, and commands. `initialize` is the one
//! bounded transaction that can conditionally extend the Router, freeze it,
//! generate capabilities, establish the connection's [`Workspace`],
//! [`Documents`], and negotiated position encoding, and run the
//! `on_initialize` lifecycle hook — all without exposing partial state
//! (ADR 0017, ADR 0018). `shutdown` and `exit` close the session. Concurrency,
//! cancellation, layers, and the outbound client arrive in later slices; here
//! each custom request runs inline.

use std::sync::Arc;

use bytes::Bytes;
use lsp_types::{InitializeParams, InitializeResult};
use serde::Serialize;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info_span, warn};

use crate::builder::{
    ConfigureInitialize, InitializeRegistrar, OnInitialize, Registrations, Router, Server,
};
use crate::codec::{decode_params, encode_body};
use crate::context::Context;
use crate::documents::Documents;
use crate::error::Error;
use crate::raw::{JsonRpcError, RawMessage, RequestId};
use crate::runtime::{Runtime, default_runtime};
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
    let runtime = default_runtime();
    let send_handle = runtime.spawn(send_loop(writer, out_rx));

    let state = server.state;
    // Every connection owns a document store; document-sync built-ins are
    // wired in a later slice, so for now it backs the handler `Context` and
    // holds the negotiated position encoding established at initialize.
    let documents = Documents::new();
    // The `Workspace` is established from `InitializeParams` during the
    // initialize transaction; until then no handler runs, so it is `None`.
    let mut workspace: Option<Workspace> = None;
    let mut phase = Phase::Uninitialized(Box::new(Pending {
        registrations: server.registrations,
        configure_initialize: server.configure_initialize,
        on_initialize: server.on_initialize,
    }));

    let outcome = loop {
        let msg = match reader.recv().await {
            Ok(msg) => msg,
            Err(TransportError::Closed) => {
                warn!("transport closed by peer before exit notification");
                break Ok(());
            }
            Err(e) => break Err(Error::Transport(e)),
        };

        match dispatch(&state, &documents, &mut workspace, &out_tx, &mut phase, msg).await {
            Flow::Continue => {}
            // `exit`, and the terminal close path a failed initialize enters,
            // both end the read-loop; the send-loop then drains what is queued.
            Flow::Exit | Flow::Close => break Ok(()),
        }
    };

    // Drop the master sender so the send-loop drains what is queued and exits.
    drop(out_tx);
    send_handle.join().await;
    outcome
}

async fn send_loop<W: TransportWriter>(mut writer: W, mut out_rx: UnboundedReceiver<RawMessage>) {
    while let Some(msg) = out_rx.recv().await {
        if let Err(e) = writer.send(msg).await {
            warn!(error = %e, "send_loop: transport write failed");
            return;
        }
    }
    if let Err(e) = writer.shutdown().await {
        warn!(error = %e, "send_loop: transport shutdown failed");
    }
}

/// The static registrations and lifecycle callbacks awaiting the initialize
/// transaction. Held only while the connection is [`Phase::Uninitialized`];
/// the transaction consumes it once, so it need not be `Clone`.
struct Pending<S> {
    registrations: Registrations<S>,
    configure_initialize: Option<ConfigureInitialize<S>>,
    on_initialize: Option<OnInitialize<S>>,
}

/// The connection's lifecycle phase. The frozen [`Router`] exists only after a
/// successful initialize transaction, so it lives inside [`Phase::Running`]
/// rather than being available up front.
enum Phase<S> {
    Uninitialized(Box<Pending<S>>),
    Running(Arc<Router<S>>),
    ShuttingDown,
}

enum Flow {
    Continue,
    Exit,
    /// The terminal close path taken after a failed initialize transaction
    /// (ADR 0018): the fixed error is already enqueued; end the session rather
    /// than returning to an uninitialized state.
    Close,
}

async fn dispatch<S>(
    state: &Arc<S>,
    documents: &Documents,
    workspace: &mut Option<Workspace>,
    out_tx: &UnboundedSender<RawMessage>,
    phase: &mut Phase<S>,
    msg: RawMessage,
) -> Flow
where
    S: Send + Sync + 'static,
{
    match msg {
        RawMessage::Request { id, method, params } => {
            let span = info_span!("request", method = %method, id = ?id);

            // Initialize precedence: until `initialize` completes, refuse
            // every other request with `ServerNotInitialized`.
            if method != "initialize" && matches!(phase, Phase::Uninitialized(_)) {
                enqueue_error(out_tx, id, LspError::ServerNotInitialized);
                return Flow::Continue;
            }
            // After `shutdown`, every request is invalid until `exit`.
            if matches!(phase, Phase::ShuttingDown) {
                enqueue_error(out_tx, id, LspError::invalid_request("invalid request"));
                return Flow::Continue;
            }

            match method.as_ref() {
                "initialize" => {
                    return initialize(
                        state, documents, workspace, out_tx, phase, &span, id, params,
                    )
                    .await;
                }
                "shutdown" => {
                    enqueue_ok(out_tx, id, &serde_json::Value::Null);
                    *phase = Phase::ShuttingDown;
                }
                other => {
                    // Precedence guarantees the connection is running here.
                    let router = match phase {
                        Phase::Running(router) => Arc::clone(router),
                        _ => {
                            enqueue_error(out_tx, id, LspError::ServerNotInitialized);
                            return Flow::Continue;
                        }
                    };
                    // Commands dispatch beneath `workspace/executeCommand`; an
                    // explicit request handler for that method is a build-time
                    // conflict, so the two never coexist.
                    if other == "workspace/executeCommand" && router.has_commands() {
                        dispatch_command(
                            state, &router, documents, workspace, out_tx, &span, id, params,
                        )
                        .await;
                    } else if let Some(handler) = router.request(other) {
                        let ctx = attach_workspace(
                            Context::for_request(
                                id.clone(),
                                span.clone(),
                                out_tx.clone(),
                                documents.clone(),
                            ),
                            workspace,
                        );
                        let result =
                            handler(Arc::clone(state), ctx, params, CancellationToken::new())
                                .instrument(span)
                                .await;
                        enqueue_encoded(out_tx, id, result);
                    } else {
                        enqueue_error(out_tx, id, LspError::MethodNotFound(other.to_string()));
                    }
                }
            }
        }
        RawMessage::Notification { method, params } => match method.as_ref() {
            "exit" => return Flow::Exit,
            other => {
                // Until `initialize` completes, drop every notification but the
                // `exit` handled above. A registered custom notification then
                // dispatches with no response; an unregistered one is ignored.
                match phase {
                    Phase::Running(router) => {
                        if let Some(handler) = router.notification(other) {
                            let span = info_span!("notification", method = %other);
                            let ctx = attach_workspace(
                                Context::for_notification(
                                    span.clone(),
                                    out_tx.clone(),
                                    documents.clone(),
                                ),
                                workspace,
                            );
                            handler(Arc::clone(state), ctx, params)
                                .instrument(span)
                                .await;
                        } else {
                            debug!(method = other, "notification ignored");
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
    phase: &mut Phase<S>,
    span: &tracing::Span,
    id: RequestId,
    params: Bytes,
) -> Flow
where
    S: Send + Sync + 'static,
{
    // A second `initialize` after the transaction has run is invalid.
    if !matches!(phase, Phase::Uninitialized(_)) {
        enqueue_error(
            out_tx,
            id,
            LspError::ServerError {
                code: -32600,
                message: "server already initialized".into(),
                data: None,
            },
        );
        return Flow::Continue;
    }

    // Malformed `initialize` params leave the transaction unspent: the client
    // may retry with a valid request, so stay uninitialized.
    let params = match decode_params::<InitializeParams>(&params) {
        Ok(params) => params,
        Err(err) => {
            enqueue_error(out_tx, id, err);
            return Flow::Continue;
        }
    };

    // Take ownership of the pending registrations and callbacks; the transaction
    // consumes them exactly once.
    let pending = match std::mem::replace(phase, Phase::ShuttingDown) {
        Phase::Uninitialized(pending) => *pending,
        // The `matches!` guard above already established this arm.
        _ => unreachable!("initialize runs only while uninitialized"),
    };
    let Pending {
        registrations,
        configure_initialize,
        on_initialize,
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
            enqueue_error(out_tx, id, LspError::internal("initialization failed"));
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
                Context::for_request(id.clone(), span.clone(), out_tx.clone(), documents.clone())
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
                    enqueue_error(out_tx, id, err);
                    return Flow::Close;
                }
            }
        }
        None => None,
    };

    enqueue_ok(
        out_tx,
        id,
        &InitializeResult {
            capabilities,
            server_info,
        },
    );
    *phase = Phase::Running(router);
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

/// Route one `workspace/executeCommand` request to the typed command table.
///
/// The engine decodes [`ExecuteCommandParams`](lsp_types::ExecuteCommandParams)
/// to select the command by name, then hands the raw argument array to the
/// erased command handler, which decodes it into the typed `Args` once. An
/// unknown command name is an invalid parameter for this method.
#[allow(clippy::too_many_arguments)]
async fn dispatch_command<S>(
    state: &Arc<S>,
    router: &Router<S>,
    documents: &Documents,
    workspace: &Option<Workspace>,
    out_tx: &UnboundedSender<RawMessage>,
    span: &tracing::Span,
    id: RequestId,
    params: Bytes,
) where
    S: Send + Sync + 'static,
{
    let params: lsp_types::ExecuteCommandParams = match decode_params(&params) {
        Ok(params) => params,
        Err(err) => {
            enqueue_error(out_tx, id, err);
            return;
        }
    };
    match router.command(&params.command) {
        Some(handler) => {
            let ctx = attach_workspace(
                Context::for_request(id.clone(), span.clone(), out_tx.clone(), documents.clone()),
                workspace,
            );
            let result = handler(
                Arc::clone(state),
                ctx,
                params.arguments,
                CancellationToken::new(),
            )
            .instrument(span.clone())
            .await;
            enqueue_encoded(out_tx, id, result);
        }
        None => enqueue_error(
            out_tx,
            id,
            LspError::invalid_params(format!("unknown command: {}", params.command)),
        ),
    }
}

/// Enqueue a success response from an already-encoded result (as produced by
/// an erased custom handler), or the mapped wire error.
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

/// Encode `value` and enqueue it as a success response. Used for lifecycle
/// replies the engine owns directly.
fn enqueue_ok<R: Serialize>(out_tx: &UnboundedSender<RawMessage>, id: RequestId, value: &R) {
    enqueue_encoded(out_tx, id, encode_body(value));
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
