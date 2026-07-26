//! Minimal protocol engine for the 0.2 `Server<S>` (PRD 0.2 slices 3–5).
//!
//! This slice serves a connection end to end for the lifecycle plus typed
//! custom requests: `initialize` establishes the running state and reports
//! capabilities computed from the frozen [`Router`](crate::builder::Router),
//! custom requests dispatch through the Router, `shutdown` and `exit` close
//! the session. Concurrency, cancellation, layers, and the outbound client
//! arrive in later slices; here each custom request runs inline.

use std::sync::Arc;

use bytes::Bytes;
use serde::Serialize;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info_span, warn};

use crate::builder::{Router, Server};
use crate::codec::{decode_params, encode_body};
use crate::context::Context;
use crate::documents::Documents;
use crate::error::Error;
use crate::raw::{JsonRpcError, RawMessage, RequestId};
use crate::runtime::{Runtime, default_runtime};
use crate::transport::{Transport, TransportError, TransportReader, TransportWriter};
use crate::{LspError, Result};

/// Drive a [`Server`] over `transport` until the peer exits, the transport
/// closes, or a transport error ends the session.
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
    let router = server.router;
    // Every connection owns a document store; document-sync built-ins are
    // wired in a later slice, so for now it only backs the handler `Context`.
    let documents = Documents::new();
    let mut lifecycle = Lifecycle::Uninitialized;

    let outcome = loop {
        let msg = match reader.recv().await {
            Ok(msg) => msg,
            Err(TransportError::Closed) => {
                warn!("transport closed by peer before exit notification");
                break Ok(());
            }
            Err(e) => break Err(Error::Transport(e)),
        };

        if let Flow::Exit =
            dispatch(&state, &router, &documents, &out_tx, &mut lifecycle, msg).await
        {
            break Ok(());
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Uninitialized,
    Running,
    ShuttingDown,
}

enum Flow {
    Continue,
    Exit,
}

async fn dispatch<S>(
    state: &Arc<S>,
    router: &Router<S>,
    documents: &Documents,
    out_tx: &UnboundedSender<RawMessage>,
    lifecycle: &mut Lifecycle,
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
            if method != "initialize" && *lifecycle == Lifecycle::Uninitialized {
                enqueue_error(out_tx, id, LspError::ServerNotInitialized);
                return Flow::Continue;
            }
            // After `shutdown`, every request is invalid until `exit`.
            if *lifecycle == Lifecycle::ShuttingDown {
                enqueue_error(out_tx, id, LspError::invalid_request("invalid request"));
                return Flow::Continue;
            }

            match method.as_ref() {
                "initialize" => {
                    if *lifecycle != Lifecycle::Uninitialized {
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
                    match decode_params::<lsp_types::InitializeParams>(&params) {
                        Ok(_params) => {
                            let result = lsp_types::InitializeResult {
                                capabilities: router.capabilities(),
                                server_info: None,
                            };
                            enqueue_ok(out_tx, id, &result);
                            *lifecycle = Lifecycle::Running;
                        }
                        Err(err) => enqueue_error(out_tx, id, err),
                    }
                }
                "shutdown" => {
                    enqueue_ok(out_tx, id, &serde_json::Value::Null);
                    *lifecycle = Lifecycle::ShuttingDown;
                }
                other => match router.request(other) {
                    Some(handler) => {
                        let ctx = Context::for_request(
                            id.clone(),
                            span.clone(),
                            out_tx.clone(),
                            documents.clone(),
                        );
                        let result =
                            handler(Arc::clone(state), ctx, params, CancellationToken::new())
                                .instrument(span)
                                .await;
                        enqueue_encoded(out_tx, id, result);
                    }
                    None => {
                        enqueue_error(out_tx, id, LspError::MethodNotFound(other.to_string()));
                    }
                },
            }
        }
        RawMessage::Notification { method, .. } => match method.as_ref() {
            "exit" => return Flow::Exit,
            other => debug!(method = other, "notification ignored"),
        },
        RawMessage::Response { .. } => warn!("ignoring unexpected response"),
        RawMessage::ProtocolError { error } => {
            let _ = out_tx.send(RawMessage::ProtocolError { error });
        }
    }

    Flow::Continue
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
