use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;
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
/// owns the reader and spawns every spawned handler into a shared
/// [`JoinSet`] against `Arc<S>`. Each in-flight request is also tracked
/// in a registry keyed by `RequestId` holding its [`CancellationToken`],
/// so a `$/cancelRequest` can trigger that token and drop the handler
/// future at its next yield — the wire then carries a `-32800
/// RequestCancelled` response (ADR 0007). On `exit`, the read-loop aborts
/// the entire [`JoinSet`] so no in-flight handler is awaited to
/// completion (issue #4). Responses and outgoing notifications all flow
/// through the same channel — the send-loop is the sole writer to the
/// transport.
pub(crate) async fn run<S, T>(server: S, transport: T, concurrency_limit: usize) -> Result<Outcome>
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
    let permits = Arc::new(Semaphore::new(concurrency_limit));
    // Every spawned handler lives here. Requests also self-remove from
    // `registry` on completion; this set additionally lets `exit` abort
    // them all at once.
    let mut tasks: JoinSet<()> = JoinSet::new();

    loop {
        // Reap finished handlers so the set doesn't grow unbounded over a
        // long session (each completed task already released its permit).
        while tasks.try_join_next().is_some() {}

        let msg = match reader.recv().await {
            Ok(msg) => msg,
            Err(TransportError::Closed) => {
                // Peer disconnected before `exit`. Drain whatever
                // in-flight handlers have already queued, then return;
                // unlike `exit`, we let outstanding handlers finish
                // rather than abort them.
                warn!("transport closed by peer before exit notification");
                drop(out_tx);
                let _ = send_handle.await;
                return Ok(Outcome::TransportClosed);
            }
            Err(e) => return Err(Error::Transport(e)),
        };

        let flow = dispatch(
            &server, &out_tx, &state, &registry, &permits, &mut tasks, msg,
        )
        .await?;
        if let Flow::Exit(code) = flow {
            // `exit` means "stop now": abort every in-flight handler and
            // wait for them to drop (which releases their clones of the
            // outgoing sender). Then drop our master sender so the
            // send-loop drains whatever was already queued and exits
            // cleanly, and hand the exit code back to the entry point —
            // which decides whether to terminate the process (binary) or
            // simply return (library / tests).
            tasks.shutdown().await;
            drop(out_tx);
            let _ = send_handle.await;
            return Ok(Outcome::Exit(code));
        }
    }
}

/// What ended the dispatcher's read-loop. The entry point maps this to a
/// process exit for a real binary (`StdioBuilder::serve`) or simply
/// returns it for the library escape hatch (`lspf::serve`), so the same
/// dispatcher is testable in-process without a `process::exit` that would
/// take the test runner down with it.
pub(crate) enum Outcome {
    /// The peer closed the transport before sending `exit`.
    TransportClosed,
    /// An `exit` notification was processed; carries the LSP exit code
    /// (0 if `shutdown` preceded it, else 1).
    Exit(i32),
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

/// In-flight request registry: maps each spawned request's `RequestId`
/// to its [`CancellationToken`]. The entry is removed atomically by
/// whichever happens first — the handler completing, or a
/// `$/cancelRequest` arriving for its id — and that removal arbitrates
/// who writes the single wire response. The handler's [`JoinHandle`]
/// lives in the read-loop's [`JoinSet`], not here.
type Registry = Arc<Mutex<HashMap<RequestId, CancellationToken>>>;

#[derive(serde::Deserialize)]
struct CancelParams {
    id: RequestId,
}

async fn dispatch<S>(
    server: &Arc<S>,
    out_tx: &UnboundedSender<RawMessage>,
    state: &SharedState,
    registry: &Registry,
    permits: &Arc<Semaphore>,
    tasks: &mut JoinSet<()>,
    msg: RawMessage,
) -> Result<Flow>
where
    S: LanguageServer,
{
    match msg {
        RawMessage::Request { id, method, params } => {
            let span = info_span!("request", method = %method, id = ?id);

            // Initialize precedence: until `initialize` completes, every
            // other request is refused with `ServerNotInitialized`
            // *before* any handler task is spawned (issue #4). Gating the
            // spawn step — not a post-spawn check inside the task — is
            // what keeps the guarantee under concurrent dispatch.
            if method != "initialize" && *state.lock().unwrap() == State::Uninitialized {
                enqueue_error(out_tx, id, LspError::ServerNotInitialized);
                return Ok(Flow::Continue);
            }

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
                    // Run inline (ADR 0003): the read-loop blocks here until
                    // `initialize` completes, so the `state → Running`
                    // transition is synchronous and every later message sees
                    // the post-init state. Spawning instead would let the
                    // next message be dispatched while still `Uninitialized`,
                    // defeating initialize-precedence (issue #4). A slow
                    // `initialize` stalling the read-loop is correct per the
                    // LSP spec — clients may not send other requests until it
                    // returns. initialize is therefore not cancellable; the
                    // token is a never-firing placeholder.
                    let params = parse_params(&params)?;
                    let ctx = Context::for_request(id.clone(), span.clone(), out_tx.clone());
                    let result = server
                        .initialize(&ctx, params, CancellationToken::new())
                        .instrument(span)
                        .await;
                    if result.is_ok() {
                        *state.lock().unwrap() = State::Running;
                    }
                    enqueue_value_response(out_tx, id, result.and_then(to_value));
                }
                "shutdown" => {
                    let server = Arc::clone(server);
                    let state = Arc::clone(state);
                    let permit = acquire_permit(permits).await;
                    spawn_request(
                        tasks,
                        registry,
                        out_tx,
                        span,
                        id,
                        permit,
                        move |ctx, ct| async move {
                            let result = server.shutdown(&ctx, ct).await;
                            if result.is_ok() {
                                *state.lock().unwrap() = State::ShuttingDown;
                            }
                            result.map(|()| serde_json::Value::Null)
                        },
                    );
                }
                other => {
                    // Uninitialized was already refused by the gate above,
                    // so reaching here means the server is running.
                    enqueue_error(out_tx, id, LspError::MethodNotFound(other.to_string()));
                }
            }
        }
        RawMessage::Notification { method, params } => {
            let span = info_span!("notification", method = %method);

            // Initialize precedence (LSP §Initialize): until `initialize`
            // completes, every notification is dropped except `exit`
            // (which lets an uninitialized server still shut down) and
            // `initialized` (the handshake's other half). Dropping happens
            // before any handler is spawned (issue #4).
            if method != "initialized"
                && method != "exit"
                && *state.lock().unwrap() == State::Uninitialized
            {
                debug!(method = %method, "dropping notification before initialize");
                return Ok(Flow::Continue);
            }

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
                    let permit = acquire_permit(permits).await;
                    spawn_notification(
                        tasks,
                        server,
                        out_tx,
                        span,
                        permit,
                        move |server, ctx| async move {
                            server.initialized(&ctx, params).await;
                        },
                    );
                }
                "textDocument/didOpen" => {
                    let params = parse_params(&params)?;
                    let permit = acquire_permit(permits).await;
                    spawn_notification(
                        tasks,
                        server,
                        out_tx,
                        span,
                        permit,
                        move |server, ctx| async move {
                            server.text_document_did_open(&ctx, params).await;
                        },
                    );
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
///
/// The task is spawned into the shared [`JoinSet`] so `exit` can abort it
/// along with every other in-flight handler.
fn spawn_request<F, Fut>(
    tasks: &mut JoinSet<()>,
    registry: &Registry,
    out_tx: &UnboundedSender<RawMessage>,
    span: Span,
    id: RequestId,
    permit: tokio::sync::OwnedSemaphorePermit,
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

    tasks.spawn(
        async move {
            // Hold the permit for the lifetime of the task; dropping at
            // task end (whether the body finished, was cancelled, or
            // panicked) is what releases the concurrency slot.
            let _permit = permit;
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

    registry.lock().unwrap().insert(id, ct);
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
    let token = registry.lock().unwrap().remove(&parsed.id);
    if let Some(token) = token {
        // Cancel the token (wakes polite `ct.cancelled().await`s and
        // flips `ct.is_cancelled()`) and write the wire response. The
        // spawned task's own `select!` then drops the body future at
        // its next yield — we don't abort its `JoinHandle` directly
        // because abort races with the polite path: it can drop the
        // future before the handler ever gets polled with the token
        // observed. (The task stays in the `JoinSet` and is reaped once
        // it finishes.)
        token.cancel();
        enqueue_error(out_tx, parsed.id, LspError::RequestCancelled);
    }
}

fn spawn_notification<S, F, Fut>(
    tasks: &mut JoinSet<()>,
    server: &Arc<S>,
    out_tx: &UnboundedSender<RawMessage>,
    span: tracing::Span,
    permit: tokio::sync::OwnedSemaphorePermit,
    body: F,
) where
    S: LanguageServer,
    F: FnOnce(Arc<S>, Context) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let server = Arc::clone(server);
    let out_tx = out_tx.clone();
    let span_for_task = span.clone();
    tasks.spawn(
        async move {
            let _permit = permit;
            let ctx = Context::for_notification(span_for_task, out_tx);
            body(server, ctx).await;
        }
        .instrument(span),
    );
}

/// Acquire one concurrency permit, wrapped in a span so traces show how
/// long handlers waited when the cap is hit (ADR 0012). The span opens
/// before the `acquire_owned().await` and closes the instant the permit
/// is held — its `.elapsed` is the queueing latency for this handler.
async fn acquire_permit(permits: &Arc<Semaphore>) -> tokio::sync::OwnedSemaphorePermit {
    Arc::clone(permits)
        .acquire_owned()
        .instrument(info_span!("handler.acquire_permit"))
        .await
        .expect("dispatcher semaphore is never closed")
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
