//! Lifecycle-hook coverage for the stable catalog boundary (issue #81).
//!
//! `on_initialized` runs at most once after initialization; `on_shutdown`
//! gates the transition into shutting down and leaves the connection running
//! on error; `on_exit` runs before the protocol engine records the close cause
//! that becomes the session `Outcome` and cannot change its LSP exit code.
//! Every test drives the public `Server::serve` transport seam and asserts on
//! the returned `Outcome` plus what each hook observed, so no assertion depends
//! on a scheduler delay.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use lspf::types::notification::Notification;
use lspf::{
    CancellationToken, LspError, Outcome, RawMessage, RequestId, Server, Transport, TransportError,
    TransportReader, TransportWriter,
};

/// A client-bound notification a hook can emit, proving its `Context` client
/// handle is live.
enum HookNotice {}

impl Notification for HookNotice {
    type Params = Value;
    const METHOD: &'static str = "test/hook-notice";
}

#[derive(Default)]
struct AppState {
    initialized_runs: AtomicUsize,
    shutdown_runs: AtomicUsize,
    exit_runs: AtomicUsize,
}

// --- In-memory transport -----------------------------------------------------

struct ChannelTransport {
    in_rx: mpsc::UnboundedReceiver<RawMessage>,
    out_tx: mpsc::UnboundedSender<RawMessage>,
}

struct ChannelReader {
    in_rx: mpsc::UnboundedReceiver<RawMessage>,
}

struct ChannelWriter {
    out_tx: mpsc::UnboundedSender<RawMessage>,
}

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            ChannelReader { in_rx: self.in_rx },
            ChannelWriter {
                out_tx: self.out_tx,
            },
        )
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.in_rx.recv().await.ok_or(TransportError::Closed)
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, msg: RawMessage) -> Result<(), TransportError> {
        self.out_tx.send(msg).map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

// --- Envelope helpers --------------------------------------------------------

fn request(id: i32, method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn initialize_request(id: i32) -> RawMessage {
    request(
        id,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    )
}

fn shutdown_request(id: i32) -> RawMessage {
    request(id, "shutdown", json!(null))
}

fn notification(method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn initialized() -> RawMessage {
    notification("initialized", json!({}))
}

fn exit() -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed("exit"),
        params: Bytes::from_static(b"null"),
    }
}

/// Drive `server` with `messages` in order, waiting for each request's
/// response before sending the next message. Drops the transport afterwards so
/// `serve` returns once everything is processed, and returns both the outbox
/// and the session [`Outcome`].
async fn drive<S: Send + Sync + 'static>(
    server: Server<S>,
    messages: Vec<RawMessage>,
) -> (Vec<RawMessage>, Outcome) {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let transport = ChannelTransport { in_rx, out_tx };

    let mut handle = tokio::spawn(async move { server.serve(transport).await });
    let mut server_done = false;
    let mut outbox = Vec::new();

    'messages: for msg in messages {
        let response_id = msg.id().cloned();
        // A failed initialize transaction terminates the connection, so a send
        // can legitimately race the disconnect; stop feeding the channel
        // instead of panicking on `SendError`.
        if in_tx.send(msg).is_err() {
            break;
        }
        if let Some(response_id) = response_id {
            loop {
                tokio::select! {
                    response = out_rx.recv() => {
                        if let Some(response) = response {
                            let is_response = response.id() == Some(&response_id);
                            outbox.push(response);
                            if is_response {
                                break;
                            }
                        } else {
                            (&mut handle)
                                .await
                                .expect("server task did not panic")
                                .expect("serve ended cleanly");
                            server_done = true;
                            break 'messages;
                        }
                    }
                    result = &mut handle => {
                        result
                            .expect("server task did not panic")
                            .expect("serve ended cleanly");
                        server_done = true;
                        break 'messages;
                    }
                }
            }
        }
    }
    drop(in_tx);

    let outcome = if !server_done {
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("serve returned within 2s")
            .expect("server task did not panic")
            .expect("serve ended cleanly")
    } else {
        handle
            .await
            .expect("server task did not panic")
            .expect("serve ended cleanly")
    };

    outbox.extend(std::iter::from_fn(|| out_rx.try_recv().ok()));
    (outbox, outcome)
}

fn hook_notices(outbox: &[RawMessage]) -> usize {
    outbox
        .iter()
        .filter(|m| m.method() == Some("test/hook-notice"))
        .count()
}

// --- Tests -------------------------------------------------------------------

/// A successful shutdown hook runs before the built-in transition, receives
/// the request-handler arguments, and can use its live `Context` before the
/// shutdown response is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_hook_runs_before_a_successful_transition() {
    let state = Arc::new(AppState::default());
    let server = Server::builder(Arc::clone(&state))
        .on_shutdown(
            |state, ctx, _params: (), cancellation: CancellationToken| async move {
                assert!(!cancellation.is_cancelled());
                state.shutdown_runs.fetch_add(1, Ordering::SeqCst);
                ctx.client()
                    .notify::<HookNotice>(json!({ "from": "shutdown" }))
                    .expect("the shutdown hook has a live client");
                Ok::<(), LspError>(())
            },
        )
        .build()
        .expect("server builds");

    let (outbox, outcome) = drive(
        server,
        vec![initialize_request(1), shutdown_request(2), exit()],
    )
    .await;

    assert_eq!(outcome, Outcome::Exit { code: 0 });
    assert_eq!(state.shutdown_runs.load(Ordering::SeqCst), 1);
    assert_eq!(hook_notices(&outbox), 1);
    let notice_index = outbox
        .iter()
        .position(|message| message.method() == Some("test/hook-notice"))
        .expect("the hook notice reached the wire");
    let response_index = outbox
        .iter()
        .position(|message| message.id() == Some(&RequestId::Number(2)))
        .expect("the shutdown response reached the wire");
    assert!(
        notice_index < response_index,
        "the hook completes before the successful shutdown response"
    );
}

/// A rejected shutdown returns the hook's error without changing lifecycle
/// state or consuming the hook, so a later shutdown request can retry it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_shutdown_hook_leaves_the_connection_running_for_retry() {
    let state = Arc::new(AppState::default());
    let server = Server::builder(Arc::clone(&state))
        .on_shutdown(|state, _ctx, (), _cancellation| async move {
            let attempt = state.shutdown_runs.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                Err(LspError::invalid_request("shutdown postponed"))
            } else {
                Ok(())
            }
        })
        .build()
        .expect("server builds");

    let (outbox, outcome) = drive(
        server,
        vec![
            initialize_request(1),
            shutdown_request(2),
            shutdown_request(3),
            exit(),
        ],
    )
    .await;

    let failed = outbox
        .iter()
        .find(|message| message.id() == Some(&RequestId::Number(2)))
        .expect("the first shutdown has a response");
    assert!(
        matches!(failed, RawMessage::Response { result: Err(error), .. } if error.code == -32600),
        "the hook's LSP error is returned to the client"
    );
    assert_eq!(state.shutdown_runs.load(Ordering::SeqCst), 2);
    assert_eq!(
        outcome,
        Outcome::Exit { code: 0 },
        "the second attempt succeeds because the first left the connection running"
    );
}

/// Malformed shutdown params are rejected before the typed hook; like a hook
/// error, validation failure leaves the connection running for a valid retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_shutdown_params_skip_the_hook_and_leave_the_connection_running() {
    let state = Arc::new(AppState::default());
    let server = Server::builder(Arc::clone(&state))
        .on_shutdown(|state, _ctx, (), _cancellation| async move {
            state.shutdown_runs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .build()
        .expect("server builds");

    let (outbox, outcome) = drive(
        server,
        vec![
            initialize_request(1),
            request(2, "shutdown", json!({ "unexpected": true })),
            shutdown_request(3),
            exit(),
        ],
    )
    .await;

    let errors = outbox
        .iter()
        .filter(|message| {
            matches!(message, RawMessage::Response { result: Err(error), .. } if error.code == -32602)
        })
        .count();
    assert_eq!(errors, 1, "running-state malformed params are invalid");
    assert_eq!(
        state.shutdown_runs.load(Ordering::SeqCst),
        1,
        "only the valid retry reaches the typed hook"
    );
    assert_eq!(outcome, Outcome::Exit { code: 0 });
}

/// The initialized hook runs exactly once, and only for the first
/// running-state `initialized` notification: a repeat is a no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialized_hook_runs_once_after_successful_initialize() {
    let state = Arc::new(AppState::default());
    let server = Server::builder(Arc::clone(&state))
        .on_initialized(|state, _ctx, _params| async move {
            state.initialized_runs.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .expect("server builds");

    let (outbox, outcome) = drive(
        server,
        vec![initialize_request(1), initialized(), initialized(), exit()],
    )
    .await;

    assert_eq!(outcome, Outcome::Exit { code: 1 });
    assert_eq!(
        state.initialized_runs.load(Ordering::SeqCst),
        1,
        "the hook runs once, for the first running-state initialized"
    );
    assert_eq!(hook_notices(&outbox), 0);
}

/// The initialized hook receives typed params and a live `Context`: its client
/// handle can send a notification that reaches the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialized_hook_receives_typed_params_and_a_live_context() {
    let state = Arc::new(AppState::default());
    let server = Server::builder(Arc::clone(&state))
        .on_initialized(|state, ctx, params| async move {
            state.initialized_runs.fetch_add(1, Ordering::SeqCst);
            // The hook dispatches on typed, decoded `InitializedParams`.
            let _ = params;
            let _ = ctx
                .client()
                .notify::<HookNotice>(json!({ "from": "initialized" }));
        })
        .build()
        .expect("server builds");

    let (outbox, outcome) = drive(server, vec![initialize_request(1), initialized(), exit()]).await;

    assert_eq!(outcome, Outcome::Exit { code: 1 });
    assert_eq!(state.initialized_runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        hook_notices(&outbox),
        1,
        "the hook's client notification reached the wire"
    );
}

/// An `initialized` notification received before `initialize` is ignored
/// without consuming the hook, so the later, valid notification still runs it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialized_before_initialize_is_ignored_and_does_not_consume_the_hook() {
    let state = Arc::new(AppState::default());
    let server = Server::builder(Arc::clone(&state))
        .on_initialized(|state, _ctx, _params| async move {
            state.initialized_runs.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .expect("server builds");

    let (outbox, outcome) = drive(
        server,
        vec![initialized(), initialize_request(1), initialized(), exit()],
    )
    .await;

    assert_eq!(outcome, Outcome::Exit { code: 1 });
    assert_eq!(
        state.initialized_runs.load(Ordering::SeqCst),
        1,
        "only the running-state initialized notification runs the hook"
    );
    assert_eq!(hook_notices(&outbox), 0);
}

/// An `initialized` notification after `shutdown` is ignored like any other
/// user work in the shutting-down state, without running the hook.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialized_after_shutdown_is_ignored() {
    let state = Arc::new(AppState::default());
    let server = Server::builder(Arc::clone(&state))
        .on_initialized(|state, _ctx, _params| async move {
            state.initialized_runs.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .expect("server builds");

    let (outbox, outcome) = drive(
        server,
        vec![
            initialize_request(1),
            shutdown_request(2),
            initialized(),
            exit(),
        ],
    )
    .await;

    assert_eq!(outcome, Outcome::Exit { code: 0 });
    assert_eq!(
        state.initialized_runs.load(Ordering::SeqCst),
        0,
        "initialized after shutdown never runs the hook"
    );
    assert_eq!(hook_notices(&outbox), 0);
}

/// Malformed `initialized` params are dropped without running the hook, and
/// the session continues. Both wire spellings of the empty params object —
/// `{}` and `null` — are accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_initialized_params_skip_the_hook_but_valid_empty_ones_run_it() {
    let state = Arc::new(AppState::default());
    let server = Server::builder(Arc::clone(&state))
        .on_initialized(|state, _ctx, _params| async move {
            state.initialized_runs.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .expect("server builds");

    // `initialized` with a non-object payload decodes as malformed and is
    // dropped; `null` (no params at all) is the other accepted spelling.
    let (outbox, outcome) = drive(
        server,
        vec![
            initialize_request(1),
            notification("initialized", json!(17)),
            RawMessage::Notification {
                method: Cow::Borrowed("initialized"),
                params: Bytes::from_static(b"null"),
            },
            exit(),
        ],
    )
    .await;

    assert_eq!(outcome, Outcome::Exit { code: 1 });
    assert_eq!(
        state.initialized_runs.load(Ordering::SeqCst),
        1,
        "the malformed notification is dropped; the null-params one runs the hook"
    );
    assert_eq!(hook_notices(&outbox), 0);
}

/// The exit hook runs before the engine records the close cause and observes
/// a live `Context`, yet the reported exit code stays engine-owned: 1 without
/// a preceding `shutdown`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_hook_runs_with_a_live_context_and_cannot_change_the_outcome() {
    let state = Arc::new(AppState::default());
    let server = Server::builder(Arc::clone(&state))
        .on_exit(|state, ctx| async move {
            state.exit_runs.fetch_add(1, Ordering::SeqCst);
            let _ = ctx.client().notify::<HookNotice>(json!({ "from": "exit" }));
        })
        .build()
        .expect("server builds");

    let (outbox, outcome) = drive(server, vec![initialize_request(1), exit()]).await;

    assert_eq!(
        outcome,
        Outcome::Exit { code: 1 },
        "the hook cannot override the lifecycle-derived exit code"
    );
    assert_eq!(state.exit_runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        hook_notices(&outbox),
        1,
        "the exit hook's client notification is drained before the writer shuts down"
    );
}

/// After a successful `shutdown` the exit hook still runs, and the outcome
/// still reports the engine-owned code 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_hook_runs_after_shutdown_with_code_zero() {
    let state = Arc::new(AppState::default());
    let server = Server::builder(Arc::clone(&state))
        .on_exit(|state, _ctx| async move {
            state.exit_runs.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .expect("server builds");

    let (outbox, outcome) = drive(
        server,
        vec![initialize_request(1), shutdown_request(2), exit()],
    )
    .await;

    assert_eq!(outcome, Outcome::Exit { code: 0 });
    assert_eq!(state.exit_runs.load(Ordering::SeqCst), 1);
    assert_eq!(hook_notices(&outbox), 0);
}

/// An `exit` received before `initialize` has no established Workspace to hand
/// the hook, so it is skipped and the connection still closes with code 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_before_initialize_skips_the_hook_and_reports_code_one() {
    let state = Arc::new(AppState::default());
    let server = Server::builder(Arc::clone(&state))
        .on_exit(|state, _ctx| async move {
            state.exit_runs.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .expect("server builds");

    let (outbox, outcome) = drive(server, vec![exit()]).await;

    assert_eq!(outcome, Outcome::Exit { code: 1 });
    assert_eq!(state.exit_runs.load(Ordering::SeqCst), 0);
    assert_eq!(hook_notices(&outbox), 0);
}

/// The exit hook observes the post-initialize Workspace: the same handle every
/// handler reads, already carrying the initialize params.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_hook_observes_the_established_workspace() {
    let observed_root = Arc::new(AtomicBool::new(false));
    let root_flag = Arc::clone(&observed_root);
    let server = Server::builder(AppState::default())
        .on_exit(move |_state, ctx| {
            let root_flag = Arc::clone(&root_flag);
            async move {
                root_flag.store(ctx.workspace().root_uri().is_some(), Ordering::SeqCst);
            }
        })
        .build()
        .expect("server builds");

    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let transport = ChannelTransport { in_rx, out_tx };
    let serving = tokio::spawn(async move { server.serve(transport).await });

    in_tx
        .send(request(
            1,
            "initialize",
            json!({
                "processId": null,
                "rootUri": "file:///project",
                "capabilities": {}
            }),
        ))
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), out_rx.recv())
        .await
        .expect("initialize response within 2s")
        .expect("outgoing channel open");
    assert_eq!(response.id(), Some(&RequestId::Number(1)));

    in_tx.send(exit()).unwrap();

    let outcome = tokio::time::timeout(Duration::from_secs(2), serving)
        .await
        .expect("serving returned within 2s")
        .expect("serving did not panic")
        .expect("serve ended cleanly");
    assert_eq!(outcome, Outcome::Exit { code: 1 });
    assert!(
        observed_root.load(Ordering::SeqCst),
        "the exit hook reads the Workspace established from InitializeParams"
    );
}
