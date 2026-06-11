use bytes::Bytes;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info_span, warn};

use crate::context::Context;
use crate::error::Error;
use crate::raw::{JsonRpcError, RawMessage, RequestId};
use crate::server::LanguageServer;
use crate::transport::{Transport, TransportError};
use crate::{LspError, Result};

/// Sequential lifecycle + did_open dispatcher (commit 2 — see ADR 0010
/// for the Layer/Service generalization, and ADR 0003 for the concurrent
/// spawn-based dispatch landing in commit 3+).
///
/// The dispatcher creates an unbounded outgoing channel at startup and
/// hands clones of the sender to every `Context` it builds. After each
/// handler invocation the channel is drained (`try_recv` loop) onto the
/// transport — outgoing notifications published by a handler always land
/// before the next inbound message is read.
pub(crate) async fn run<S, T>(server: S, mut transport: T) -> Result<()>
where
    S: LanguageServer,
    T: Transport,
{
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let mut state = State::Uninitialized;

    loop {
        let msg = match transport.recv().await {
            Ok(msg) => msg,
            Err(TransportError::Closed) => {
                warn!("transport closed by peer before exit notification");
                return Ok(());
            }
            Err(e) => return Err(Error::Transport(e)),
        };

        let flow = dispatch(&server, &mut transport, &out_tx, &mut state, msg).await?;
        drain_outgoing(&mut out_rx, &mut transport).await?;

        if let Flow::Exit(code) = flow {
            let _ = transport.shutdown().await;
            std::process::exit(code);
        }
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

async fn drain_outgoing<T: Transport>(
    out_rx: &mut UnboundedReceiver<RawMessage>,
    transport: &mut T,
) -> Result<()> {
    while let Ok(msg) = out_rx.try_recv() {
        transport.send(msg).await?;
    }
    Ok(())
}

async fn dispatch<S, T>(
    server: &S,
    transport: &mut T,
    out_tx: &UnboundedSender<RawMessage>,
    state: &mut State,
    msg: RawMessage,
) -> Result<Flow>
where
    S: LanguageServer,
    T: Transport,
{
    match msg {
        RawMessage::Request { id, method, params } => {
            let span = info_span!("request", method = %method, id = ?id);
            let ctx = Context::for_request(id.clone(), span.clone(), out_tx.clone());

            match method.as_ref() {
                "initialize" => {
                    if *state != State::Uninitialized {
                        send_error(
                            transport,
                            id,
                            LspError::ServerError {
                                code: -32600,
                                message: "server already initialized".into(),
                                data: None,
                            },
                        )
                        .await?;
                        return Ok(Flow::Continue);
                    }
                    let params = parse_params(&params)?;
                    let ct = CancellationToken::new();
                    let result = server.initialize(&ctx, params, ct).instrument(span).await;
                    send_result(transport, id, result).await?;
                    *state = State::Running;
                }
                "shutdown" => {
                    let ct = CancellationToken::new();
                    let result: std::result::Result<serde_json::Value, _> = server
                        .shutdown(&ctx, ct)
                        .instrument(span)
                        .await
                        .map(|_| serde_json::Value::Null);
                    send_result(transport, id, result).await?;
                    *state = State::ShuttingDown;
                }
                other => {
                    if *state == State::Uninitialized {
                        send_error(transport, id, LspError::ServerNotInitialized).await?;
                    } else {
                        send_error(transport, id, LspError::MethodNotFound(other.to_string()))
                            .await?;
                    }
                }
            }
        }
        RawMessage::Notification { method, params } => {
            let span = info_span!("notification", method = %method);
            let ctx = Context::for_notification(span.clone(), out_tx.clone());

            match method.as_ref() {
                "initialized" => {
                    let params = parse_params(&params)?;
                    server.initialized(&ctx, params).instrument(span).await;
                }
                "textDocument/didOpen" => {
                    let params = parse_params(&params)?;
                    server
                        .text_document_did_open(&ctx, params)
                        .instrument(span)
                        .await;
                }
                "exit" => {
                    server.exit(&ctx).instrument(span).await;
                    let code = if *state == State::ShuttingDown { 0 } else { 1 };
                    return Ok(Flow::Exit(code));
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

fn parse_params<P: serde::de::DeserializeOwned>(params: &Bytes) -> Result<P> {
    let bytes: &[u8] = if params.is_empty() { b"{}" } else { params };
    serde_json::from_slice(bytes).map_err(|e| LspError::invalid_params(e).into())
}

async fn send_result<T, R>(
    transport: &mut T,
    id: RequestId,
    result: std::result::Result<R, LspError>,
) -> Result<()>
where
    T: Transport,
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
    transport.send(response).await?;
    Ok(())
}

async fn send_error<T: Transport>(transport: &mut T, id: RequestId, err: LspError) -> Result<()> {
    let response = RawMessage::Response {
        id,
        result: Err(JsonRpcError {
            code: err.code(),
            message: err.message(),
            data: err.data().cloned(),
        }),
    };
    transport.send(response).await?;
    Ok(())
}

impl Error {
    fn from_serde(e: serde_json::Error) -> Self {
        Error::Lsp(LspError::internal(format!("serialization failed: {e}")))
    }
}
