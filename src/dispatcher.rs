use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info_span, warn};

use crate::context::Context;
use crate::error::Error;
use crate::raw::{JsonRpcError, RawMessage, RequestId};
use crate::server::LanguageServer;
use crate::transport::{Transport, TransportError, TransportReader, TransportWriter};
use crate::{LspError, Result};

/// Concurrent dispatcher (ADR 0003 + addendum, ADR 0015).
///
/// At startup, the transport is split into a reader half and a writer
/// half. The writer half moves into a dedicated send-loop task that
/// drains an `unbounded_channel` of outgoing messages. The read-loop
/// owns the reader, advances the lifecycle state machine, and either
/// runs lifecycle handlers (`initialize`, `shutdown`, `exit`) inline
/// or spawns every other handler against `Arc<S>`. Responses and
/// outgoing notifications all flow through the same channel — the
/// send-loop is the sole writer to the transport.
pub(crate) async fn run<S, T>(server: S, transport: T) -> Result<()>
where
    S: LanguageServer,
    T: Transport,
{
    let (mut reader, writer) = transport.split();
    let server = Arc::new(server);
    let (out_tx, out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let send_handle = tokio::spawn(send_loop(writer, out_rx));

    let mut state = State::Uninitialized;
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

        let flow = dispatch(&server, &out_tx, &mut state, msg).await?;
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

enum Flow {
    Continue,
    Exit(i32),
}

async fn dispatch<S>(
    server: &Arc<S>,
    out_tx: &UnboundedSender<RawMessage>,
    state: &mut State,
    msg: RawMessage,
) -> Result<Flow>
where
    S: LanguageServer,
{
    match msg {
        RawMessage::Request { id, method, params } => {
            let span = info_span!("request", method = %method, id = ?id);
            let ctx = Context::for_request(id.clone(), span.clone(), out_tx.clone());

            match method.as_ref() {
                "initialize" => {
                    if *state != State::Uninitialized {
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
                    let ct = CancellationToken::new();
                    let result = server.initialize(&ctx, params, ct).instrument(span).await;
                    enqueue_response(out_tx, id, result)?;
                    *state = State::Running;
                }
                "shutdown" => {
                    let ct = CancellationToken::new();
                    let result: std::result::Result<serde_json::Value, _> = server
                        .shutdown(&ctx, ct)
                        .instrument(span)
                        .await
                        .map(|_| serde_json::Value::Null);
                    enqueue_response(out_tx, id, result)?;
                    *state = State::ShuttingDown;
                }
                other => {
                    if *state == State::Uninitialized {
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
                    let code = if *state == State::ShuttingDown { 0 } else { 1 };
                    return Ok(Flow::Exit(code));
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

fn enqueue_response<R>(
    out_tx: &UnboundedSender<RawMessage>,
    id: RequestId,
    result: std::result::Result<R, LspError>,
) -> Result<()>
where
    R: serde::Serialize,
{
    let response = match result {
        Ok(value) => RawMessage::Response {
            id,
            result: Ok(Bytes::from(
                serde_json::to_vec(&value).map_err(Error::from_serde)?,
            )),
        },
        Err(err) => RawMessage::Response {
            id,
            result: Err(JsonRpcError {
                code: err.code(),
                message: err.message(),
                data: err.data().cloned(),
            }),
        },
    };
    let _ = out_tx.send(response);
    Ok(())
}

fn enqueue_error(out_tx: &UnboundedSender<RawMessage>, id: RequestId, err: LspError) {
    let _ = out_tx.send(RawMessage::Response {
        id,
        result: Err(JsonRpcError {
            code: err.code(),
            message: err.message(),
            data: err.data().cloned(),
        }),
    });
}

impl Error {
    fn from_serde(e: serde_json::Error) -> Self {
        Error::Lsp(LspError::internal(format!("serialization failed: {e}")))
    }
}
