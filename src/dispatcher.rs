use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, debug, info_span, warn};

use crate::context::Context;
use crate::error::Error;
use crate::raw::{JsonRpcError, RawMessage, RequestId};
use crate::server::LanguageServer;
use crate::transport::{Transport, TransportError, TransportReader, TransportWriter};
use crate::{LspError, Result};

/// Concurrent dispatcher (ADR 0003 + addendum, ADR 0007, ADR 0015).
///
/// At startup, the transport is split into a reader half and a writer
/// half. The writer half moves into a dedicated send-loop task that
/// drains an `unbounded_channel` of outgoing messages. The read-loop
/// owns the reader and spawns every request and non-lifecycle
/// notification handler against `Arc<S>`. Each spawned request is
/// tracked in an in-flight registry keyed by `RequestId`, so a
/// `$/cancelRequest` notification can trigger the handler's
/// [`CancellationToken`] and drop the handler future at its next yield
/// — the wire then carries a `-32800 RequestCancelled` response (ADR
/// 0007). Responses and outgoing notifications all flow through the
/// same channel — the send-loop is the sole writer to the transport.
pub(crate) async fn run<S, T>(server: S, transport: T) -> Result<()>
where
    S: LanguageServer,
    T: Transport,
{
    let (mut reader, writer) = transport.split();
    let server = Arc::new(server);
    let (out_tx, out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let send_handle = tokio::spawn(send_loop(writer, out_rx));

    let state: SharedState = Arc::new(Mutex::new(State::Uninitialized));
    let registry: Registry = Arc::new(Mutex::new(HashMap::new()));

    loop {
        let msg = match reader.recv().await {
            Ok(msg) => msg,
            Err(TransportError::Closed) => {
                warn!("transport closed by peer before exit notification");
                drop(out_tx);
                let _ = send_handle.await;
                return Ok(());
            }
            Err(e) => return Err(Error::Transport(e)),
        };

        let flow = dispatch(&server, &out_tx, &state, &registry, msg).await?;
        if let Flow::Exit(code) = flow {
            // Drop our master sender so the send-loop can drain on its own
            // once any in-flight handler tasks release their clones; then
            // bail out via process::exit per LSP semantics. Spawned
            // handlers and the send-loop die with the process — issue #4
            // tightens lifecycle ordering on top of this.
            drop(out_tx);
            let _ = send_handle.await;
            std::process::exit(code);
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Uninitialized,
    Running,
    ShuttingDown,
}

type SharedState = Arc<Mutex<State>>;

enum Flow {
    Continue,
    Exit(i32),
}

/// Entry in the in-flight registry: the task running the handler plus
/// the cancellation token wired into it. Removed atomically by
/// whichever happens first — the handler completing, or a
/// `$/cancelRequest` arriving for its id.
struct InFlight {
    handle: JoinHandle<()>,
    token: CancellationToken,
}

type Registry = Arc<Mutex<HashMap<RequestId, InFlight>>>;

#[derive(serde::Deserialize)]
struct CancelParams {
    id: RequestId,
}

async fn dispatch<S>(
    server: &Arc<S>,
    out_tx: &UnboundedSender<RawMessage>,
    state: &SharedState,
    registry: &Registry,
    msg: RawMessage,
) -> Result<Flow>
where
    S: LanguageServer,
{
    match msg {
        RawMessage::Request { id, method, params } => {
            let span = info_span!("request", method = %method, id = ?id);

            match method.as_ref() {
                "initialize" => {
                    if *state.lock().unwrap() != State::Uninitialized {
                        enqueue_error(
                            out_tx,
                            id,
                            LspError::ServerError {
                                code: -32600,
                                message: "server already initialized".into(),
                                data: None,
                            },
                        );
                        return Ok(Flow::Continue);
                    }
                    let params = parse_params(&params)?;
                    let server = Arc::clone(server);
                    let state = Arc::clone(state);
                    spawn_request(registry, out_tx, span, id, move |ctx, ct| async move {
                        let result = server.initialize(&ctx, params, ct).await;
                        if result.is_ok() {
                            *state.lock().unwrap() = State::Running;
                        }
                        result.and_then(to_value)
                    });
                }
                "shutdown" => {
                    let server = Arc::clone(server);
                    let state = Arc::clone(state);
                    spawn_request(registry, out_tx, span, id, move |ctx, ct| async move {
                        let result = server.shutdown(&ctx, ct).await;
                        if result.is_ok() {
                            *state.lock().unwrap() = State::ShuttingDown;
                        }
                        result.map(|()| serde_json::Value::Null)
                    });
                }
                other => {
                    let snapshot = *state.lock().unwrap();
                    if snapshot == State::Uninitialized {
                        enqueue_error(out_tx, id, LspError::ServerNotInitialized);
                    } else {
                        enqueue_error(out_tx, id, LspError::MethodNotFound(other.to_string()));
                    }
                }
            }
        }
        RawMessage::Notification { method, params } => {
            let span = info_span!("notification", method = %method);

            match method.as_ref() {
                "exit" => {
                    let ctx = Context::for_notification(span.clone(), out_tx.clone());
                    server.exit(&ctx).instrument(span).await;
                    let code = if *state.lock().unwrap() == State::ShuttingDown {
                        0
                    } else {
                        1
                    };
                    return Ok(Flow::Exit(code));
                }
                "$/cancelRequest" => {
                    handle_cancel(registry, out_tx, &params);
                }
                "initialized" => {
                    let params = parse_params(&params)?;
                    spawn_notification(server, out_tx, span, move |server, ctx| async move {
                        server.initialized(&ctx, params).await;
                    });
                }
                "textDocument/didOpen" => {
                    let params = parse_params(&params)?;
                    spawn_notification(server, out_tx, span, move |server, ctx| async move {
                        server.text_document_did_open(&ctx, params).await;
                    });
                }
                other => {
                    debug!(method = other, "unhandled notification");
                }
            }
        }
        RawMessage::Response { .. } => {
            warn!("ignoring unexpected response");
        }
    }

    Ok(Flow::Continue)
}

/// Spawn a request handler on its own tokio task and register it for
/// cancellation. The `body` closure receives the per-request
/// [`Context`] and the live [`CancellationToken`]; its return value is
/// the wire response (serialised JSON value or [`LspError`]).
///
/// Inside the spawned task, the body races against `ct.cancelled()` so
/// that a triggered token both (a) lets polite handlers observe it via
/// `ct.is_cancelled()` / `ct.cancelled().await` and (b) drops the body
/// future at its next yield point if the handler ignores the token —
/// the cooperative equivalent of [`tokio::task::JoinHandle::abort`] but
/// without racing the polite path's own completion. On completion the
/// task tries to remove its own entry from the registry: if the entry
/// is still there, it writes the response; if `$/cancelRequest`
/// already removed it (and wrote `-32800`), the task's response is
/// dropped silently.
fn spawn_request<F, Fut>(
    registry: &Registry,
    out_tx: &UnboundedSender<RawMessage>,
    span: Span,
    id: RequestId,
    body: F,
) where
    F: FnOnce(Context, CancellationToken) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = std::result::Result<serde_json::Value, LspError>>
        + Send
        + 'static,
{
    let ct = CancellationToken::new();
    let ct_for_handler = ct.clone();
    let ct_for_select = ct.clone();
    let registry_for_task = Arc::clone(registry);
    let out_tx_for_task = out_tx.clone();
    let id_for_task = id.clone();
    let id_for_ctx = id.clone();
    let span_for_ctx = span.clone();
    let out_tx_for_ctx = out_tx.clone();

    let handle = tokio::spawn(
        async move {
            let ctx = Context::for_request(id_for_ctx, span_for_ctx, out_tx_for_ctx);
            let result = tokio::select! {
                // `biased`: poll the body before the cancel branch.
                // When the token fires, both branches wake; biased
                // gives a polite handler one extra poll to advance
                // past its `ct.cancelled().await` and finish, so the
                // observation is deterministic. An impolite body that
                // returns `Pending` still hands control to the cancel
                // branch on the same iteration.
                biased;
                r = body(ctx, ct_for_handler) => r,
                _ = ct_for_select.cancelled() => Err(LspError::RequestCancelled),
            };
            let still_present = registry_for_task
                .lock()
                .unwrap()
                .remove(&id_for_task)
                .is_some();
            if still_present {
                enqueue_value_response(&out_tx_for_task, id_for_task, result);
            }
        }
        .instrument(span),
    );

    registry
        .lock()
        .unwrap()
        .insert(id, InFlight { handle, token: ct });
}

fn handle_cancel(registry: &Registry, out_tx: &UnboundedSender<RawMessage>, params: &Bytes) {
    let bytes: &[u8] = if params.is_empty() { b"{}" } else { params };
    let parsed: CancelParams = match serde_json::from_slice(bytes) {
        Ok(p) => p,
        Err(e) => {
            debug!(error = %e, "ignoring malformed $/cancelRequest");
            return;
        }
    };
    let entry = registry.lock().unwrap().remove(&parsed.id);
    if let Some(entry) = entry {
        // Cancel the token (wakes polite `ct.cancelled().await`s and
        // flips `ct.is_cancelled()`) and write the wire response. The
        // spawned task's own `select!` then drops the body future at
        // its next yield — we don't call `JoinHandle::abort` directly
        // because abort races with the polite path: it can drop the
        // future before the handler ever gets polled with the token
        // observed.
        entry.token.cancel();
        enqueue_error(out_tx, parsed.id, LspError::RequestCancelled);
        drop(entry.handle);
    }
}

fn spawn_notification<S, F, Fut>(
    server: &Arc<S>,
    out_tx: &UnboundedSender<RawMessage>,
    span: tracing::Span,
    body: F,
) where
    S: LanguageServer,
    F: FnOnce(Arc<S>, Context) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let server = Arc::clone(server);
    let out_tx = out_tx.clone();
    let span_for_task = span.clone();
    tokio::spawn(
        async move {
            let ctx = Context::for_notification(span_for_task, out_tx);
            body(server, ctx).await;
        }
        .instrument(span),
    );
}

fn parse_params<P: serde::de::DeserializeOwned>(params: &Bytes) -> Result<P> {
    let bytes: &[u8] = if params.is_empty() { b"{}" } else { params };
    serde_json::from_slice(bytes).map_err(|e| LspError::invalid_params(e).into())
}

fn to_value<R: serde::Serialize>(value: R) -> std::result::Result<serde_json::Value, LspError> {
    serde_json::to_value(value)
        .map_err(|e| LspError::internal(format!("serialization failed: {e}")))
}

fn enqueue_value_response(
    out_tx: &UnboundedSender<RawMessage>,
    id: RequestId,
    result: std::result::Result<serde_json::Value, LspError>,
) {
    let response = match result {
        Ok(value) => match serde_json::to_vec(&value) {
            Ok(bytes) => RawMessage::Response {
                id,
                result: Ok(Bytes::from(bytes)),
            },
            Err(e) => error_response(
                id,
                &LspError::internal(format!("serialization failed: {e}")),
            ),
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
