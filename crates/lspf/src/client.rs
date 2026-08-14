use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use lsp_types::notification::{
    LogMessage, LogTrace, Notification, Progress, PublishDiagnostics, ShowMessage,
};
use lsp_types::request::{ApplyWorkspaceEdit, Request, ShowDocument, ShowMessageRequest};
use lsp_types::{
    ApplyWorkspaceEditParams, ApplyWorkspaceEditResponse, LogMessageParams, LogTraceParams,
    MessageActionItem, ProgressParams, PublishDiagnosticsParams, ShowDocumentParams,
    ShowDocumentResult, ShowMessageParams, ShowMessageRequestParams, TraceValue,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::error::ClientError;
use crate::raw::{JsonRpcError, RawMessage, RequestId};
use crate::workspace::SharedTrace;

/// A response completion value: either raw success bytes or a JSON-RPC error.
type PendingResult = std::result::Result<Bytes, JsonRpcError>;

/// What a pending outbound request eventually resolves to.
pub(crate) enum PendingOutcome {
    /// The peer answered, either with raw success bytes or a JSON-RPC error.
    Response(PendingResult),
    /// The session closed before the peer answered.
    Cancelled,
}

/// The engine-owned outbound pending-request registry and ID allocator.
///
/// Shared through an `Arc` between the `ProtocolEngine` (which inserts entries
/// before enqueue and completes them on response arrival or session close) and
/// `Client` handles (which await their specific entry).
#[derive(Clone, Default)]
pub(crate) struct OutboundRegistry {
    inner: Arc<Mutex<OutboundInner>>,
}

struct OutboundInner {
    /// Pending requests keyed by their outbound ID.
    pending: HashMap<u32, oneshot::Sender<PendingOutcome>>,
    /// Monotonically increasing counter; the next ID to try allocating.
    next_id: u32,
    /// Set by `close_all()`. Once set, `insert()` refuses to create new
    /// pending entries: any handler that starts a new outbound request after
    /// the registry has been drained fails fast instead of enqueuing an entry
    /// that would never be completed.
    closed: bool,
}

impl Default for OutboundInner {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
            next_id: 1,
            closed: false,
        }
    }
}

/// Outcome of allocating a new pending outbound-request entry.
pub(crate) enum InsertOutcome {
    /// The entry was created; the caller should enqueue the request and await
    /// the receiver.
    Inserted(u32, oneshot::Receiver<PendingOutcome>),
    /// The registry has been closed (see [`OutboundRegistry::close_all`]) and
    /// no longer accepts new pending entries.
    Closed,
    /// The positive `i32` ID space is exhausted.
    Exhausted,
}

/// RAII guard that removes a pending outbound-request entry from the registry
/// when dropped. Guards against cancelled or failed `request()` futures that
/// would otherwise leak entries in the pending map indefinitely.
///
/// When the guarded request actually reached the wire (was enqueued), dropping
/// the guard also tells the peer it was cancelled with a single typed
/// `$/cancelRequest` notification. Requests that failed before enqueue never
/// emit one.
struct PendingGuard {
    client: Client,
    id: u32,
    enqueued: bool,
}

impl PendingGuard {
    fn new(client: Client, id: u32) -> Self {
        Self {
            client,
            id,
            enqueued: false,
        }
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        // Only a still-present entry means the request never completed. Remove
        // it and, if it reached the wire, cancel it with the peer. Errors are
        // ignored: the connection may already be closing.
        if self.client.outbound.remove(self.id).is_some() && self.enqueued {
            let _ =
                self.client
                    .notify::<lsp_types::notification::Cancel>(lsp_types::CancelParams {
                        id: lsp_types::NumberOrString::Number(self.id as i32),
                    });
        }
    }
}

impl OutboundRegistry {
    /// Allocate the next outbound request ID (starting at 1) and store the
    /// completion sender.
    ///
    /// IDs are allocated monotonically and never reused, so a response for an
    /// abandoned request can never complete a later request. Once the positive
    /// `i32` ID space is exhausted, returns [`InsertOutcome::Exhausted`]. Once
    /// [`OutboundRegistry::close_all`] has run, returns
    /// [`InsertOutcome::Closed`] instead of creating an entry that could never
    /// be completed — the closed check and the drain in `close_all` share the
    /// same lock, so there is no window where a new entry is silently created
    /// after the registry has been drained.
    ///
    /// Returns the allocated ID and the receiver the caller should await.
    pub(crate) fn insert(&self) -> InsertOutcome {
        let mut inner = self.inner.lock().unwrap();
        if inner.closed {
            return InsertOutcome::Closed;
        }
        let id = inner.next_id;
        if id > i32::MAX as u32 {
            return InsertOutcome::Exhausted;
        }
        inner.next_id = id + 1;
        let (tx, rx) = oneshot::channel();
        inner.pending.insert(id, tx);
        InsertOutcome::Inserted(id, rx)
    }

    /// Remove and complete the pending entry for `id` with `result`.
    ///
    /// If no entry exists (unknown, duplicate, or late response), returns
    /// `false` and leaves all other entries intact.
    pub(crate) fn complete(&self, id: u32, result: PendingResult) -> bool {
        let tx = self.inner.lock().unwrap().pending.remove(&id);
        if let Some(tx) = tx {
            // Receiver may be gone if the caller was cancelled; ignore.
            let _ = tx.send(PendingOutcome::Response(result));
            true
        } else {
            false
        }
    }

    /// Remove the pending entry for `id` without completing it.
    ///
    /// Used when encoding or enqueue fails immediately after `insert`, and by
    /// the pending guard on abandonment.
    pub(crate) fn remove(&self, id: u32) -> Option<oneshot::Sender<PendingOutcome>> {
        self.inner.lock().unwrap().pending.remove(&id)
    }

    /// Complete every remaining pending entry with [`PendingOutcome::Cancelled`],
    /// clear the registry, and permanently close it to new entries.
    ///
    /// After this call, [`OutboundRegistry::insert`] returns
    /// [`InsertOutcome::Closed`] instead of creating new pending entries, so a
    /// handler that starts a new outbound request after the drain fails fast
    /// with `ClientError::ConnectionClosed` rather than enqueuing a request
    /// that would never receive a response. Awaiting `request()` futures that
    /// were already pending observe [`ClientError::Cancelled`].
    pub(crate) fn close_all(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.closed = true;
        let entries: HashMap<u32, oneshot::Sender<PendingOutcome>> =
            std::mem::take(&mut inner.pending);
        drop(inner);
        for tx in entries.into_values() {
            let _ = tx.send(PendingOutcome::Cancelled);
        }
    }

    /// Override the next candidate ID (test-only, for exhaustion coverage).
    #[cfg(test)]
    pub(crate) fn set_next_id(&self, id: u32) {
        self.inner.lock().unwrap().next_id = id;
    }

    /// Number of entries currently pending (test-only, for leak assertions).
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.inner.lock().unwrap().pending.len()
    }
}

#[derive(Debug, Clone, Copy)]
enum Phase {
    Open,
    ConnectionClosed,
    OutboundClosed,
}

struct ClientState {
    phase: Mutex<Phase>,
    outbound_closing: CancellationToken,
}

/// The payload of a `telemetry/event` notification.
///
/// Notification params on the wire are structured JSON — an object or an
/// array — so this type has no primitive representation: a bare string,
/// number, or boolean payload is rejected by the type system at the call
/// site. Serialization is transparent: the object or array is sent exactly
/// as constructed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TelemetryEventParams {
    /// A JSON object payload.
    Object(serde_json::Map<String, serde_json::Value>),
    /// A JSON array payload.
    Array(Vec<serde_json::Value>),
}

impl From<serde_json::Map<String, serde_json::Value>> for TelemetryEventParams {
    fn from(object: serde_json::Map<String, serde_json::Value>) -> Self {
        Self::Object(object)
    }
}

impl From<Vec<serde_json::Value>> for TelemetryEventParams {
    fn from(array: Vec<serde_json::Value>) -> Self {
        Self::Array(array)
    }
}

/// The `telemetry/event` notification with the object-or-array params type.
/// `lsp_types::notification::TelemetryEvent` types its params as
/// `serde_json::Value`, which would admit primitive payloads, so the helper
/// goes through this re-typed notification instead.
enum TelemetryEvent {}

impl Notification for TelemetryEvent {
    type Params = TelemetryEventParams;
    const METHOD: &'static str = <lsp_types::notification::TelemetryEvent as Notification>::METHOD;
}

/// A cloneable typed handle for messages sent to the current LSP client.
///
/// A `Client` is connection-scoped. It does not expose the connection's
/// outbound queue or protocol registries; cloning it only clones a cheap
/// handle into facilities owned by the protocol engine.
#[derive(Clone)]
pub struct Client {
    outgoing: UnboundedSender<RawMessage>,
    outbound: OutboundRegistry,
    state: Arc<ClientState>,
    trace: SharedTrace,
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    pub(crate) fn new(outgoing: UnboundedSender<RawMessage>, outbound: OutboundRegistry) -> Self {
        Self {
            outgoing,
            outbound,
            state: Arc::new(ClientState {
                phase: Mutex::new(Phase::Open),
                outbound_closing: CancellationToken::new(),
            }),
            trace: SharedTrace::default(),
        }
    }

    /// Encode and enqueue one typed server-to-client notification.
    ///
    /// Notifications are fire-and-forget: this method returns synchronously,
    /// allocates no request ID, and creates no pending request entry.
    pub fn notify<N>(&self, params: N::Params) -> Result<(), ClientError>
    where
        N: Notification,
    {
        {
            let mut phase = self.state.phase.lock().unwrap();
            self.ensure_open(&mut phase)?;
        }

        let params = serde_json::to_vec(&params).map_err(ClientError::Serialize)?;
        let message = RawMessage::Notification {
            method: Cow::Borrowed(N::METHOD),
            params: Bytes::from(params),
        };

        let mut phase = self.state.phase.lock().unwrap();
        self.ensure_open(&mut phase)?;
        if self.outgoing.send(message).is_err() {
            *phase = Phase::OutboundClosed;
            self.state.outbound_closing.cancel();
            return Err(ClientError::OutboundClosed);
        }
        Ok(())
    }

    /// The shared enqueue path of the named outgoing helpers: any
    /// serialization or enqueue failure is reported through `tracing` and
    /// still returned to the caller.
    fn notify_logged<N>(&self, params: N::Params) -> Result<(), ClientError>
    where
        N: Notification,
    {
        self.notify::<N>(params).inspect_err(|error| {
            warn!(method = N::METHOD, %error, "outgoing client notification failed");
        })
    }

    /// Push diagnostics to the client with `textDocument/publishDiagnostics`
    /// (LSP 3.17), fire-and-forget.
    ///
    /// The params are sent exactly as provided: diagnostics are not cached,
    /// deduplicated, or rewritten, the caller-provided `version` is
    /// preserved, and closing a document never clears them automatically.
    pub fn publish_diagnostics(&self, params: PublishDiagnosticsParams) -> Result<(), ClientError> {
        self.notify_logged::<PublishDiagnostics>(params)
    }

    /// Ask the client to display a message to the user with
    /// `window/showMessage` (LSP 3.17), fire-and-forget.
    pub fn show_message(&self, params: ShowMessageParams) -> Result<(), ClientError> {
        self.notify_logged::<ShowMessage>(params)
    }

    /// Log a message to the client's log channel with `window/logMessage`
    /// (LSP 3.17), fire-and-forget.
    ///
    /// The message goes to the client only; it is not duplicated into the
    /// server's local `tracing` stream.
    pub fn log_message(&self, params: LogMessageParams) -> Result<(), ClientError> {
        self.notify_logged::<LogMessage>(params)
    }

    /// Send a trace message with `$/logTrace` (LSP 3.17), gated on the
    /// connection's current trace level.
    ///
    /// With the level `Off` — the initial value until the client sends
    /// `$/setTrace` — nothing is enqueued and the call returns `Ok(())`.
    /// With `Messages` or `Verbose` the params are sent exactly as provided;
    /// sending never changes the level.
    pub fn log_trace(&self, params: LogTraceParams) -> Result<(), ClientError> {
        if self.trace.get() == TraceValue::Off {
            return Ok(());
        }
        self.notify_logged::<LogTrace>(params)
    }

    /// Ask the client to log a telemetry event with `telemetry/event`
    /// (LSP 3.17), fire-and-forget.
    ///
    /// The payload is a [`TelemetryEventParams`] — a JSON object or array —
    /// so primitive JSON values are rejected by the type system.
    pub fn telemetry_event(&self, params: TelemetryEventParams) -> Result<(), ClientError> {
        self.notify_logged::<TelemetryEvent>(params)
    }

    /// Report progress to the client with `$/progress` (LSP 3.17),
    /// fire-and-forget.
    pub fn progress(&self, params: ProgressParams) -> Result<(), ClientError> {
        self.notify_logged::<Progress>(params)
    }

    /// Encode and enqueue one typed server-to-client request, then await the
    /// correlated response.
    ///
    /// The method allocates a never-reused outbound ID, inserts the pending
    /// decoder before enqueue, and returns a `Future` that resolves once the
    /// engine delivers the matching response. Responses may arrive in any
    /// order. If the future is dropped before the response arrives, the
    /// pending entry is removed and the peer is told the request was cancelled
    /// with a `$/cancelRequest` notification.
    ///
    /// # Errors
    ///
    /// - [`ClientError::ConnectionClosed`] or [`ClientError::OutboundClosed`]
    ///   if the session is already closing, or if the engine's close operation
    ///   has already drained the outbound registry (e.g. this request started
    ///   after the connection began closing).
    /// - [`ClientError::IdExhausted`] if the outbound ID space is exhausted.
    /// - [`ClientError::Serialize`] if the params cannot be encoded.
    /// - [`ClientError::Cancelled`] if the session closes before the peer
    ///   answers.
    /// - [`ClientError::Remote`] if the peer replies with a JSON-RPC error.
    /// - [`ClientError::Deserialize`] if the success result cannot be decoded.
    pub async fn request<R>(&self, params: R::Params) -> Result<R::Result, ClientError>
    where
        R: Request,
    {
        // 1. Reject early if not open.
        {
            let mut phase = self.state.phase.lock().unwrap();
            self.ensure_open(&mut phase)?;
        }

        // 2. Serialize params before touching the registry (no cleanup needed
        //    on encode failure before insert).
        let params_bytes = serde_json::to_vec(&params).map_err(ClientError::Serialize)?;

        // 3. Allocate a never-reused ID and insert the pending entry. If the
        //    registry was already closed by `close_all()` (e.g. because a
        //    prior in-flight handler is still draining while a new one
        //    starts), fail fast instead of enqueuing a request that would
        //    never receive a response.
        let (id, rx) = match self.outbound.insert() {
            InsertOutcome::Inserted(id, rx) => (id, rx),
            InsertOutcome::Closed => return Err(ClientError::ConnectionClosed),
            InsertOutcome::Exhausted => return Err(ClientError::IdExhausted),
        };
        // The guard removes the pending entry if this future is dropped before
        // a response arrives (e.g. due to caller cancellation), and tells the
        // peer about the cancellation once the request reached the wire.
        let mut guard = PendingGuard::new(self.clone(), id);

        // 4. Enqueue. On any failure remove the pending entry and report the
        //    error; the request never reached the wire, so no cancellation
        //    notification is sent.
        let message = RawMessage::Request {
            id: RequestId::Number(id as i32),
            method: Cow::Borrowed(R::METHOD),
            params: Bytes::from(params_bytes),
        };

        let enqueued = {
            let mut phase = self.state.phase.lock().unwrap();
            match self.ensure_open(&mut phase) {
                Err(e) => {
                    // Remove the entry we just inserted.
                    drop(self.outbound.remove(id));
                    return Err(e);
                }
                Ok(()) => {
                    if self.outgoing.send(message).is_err() {
                        *phase = Phase::OutboundClosed;
                        self.state.outbound_closing.cancel();
                        false
                    } else {
                        true
                    }
                }
            }
        };

        if !enqueued {
            // Outbound closed during send: remove pending, report error.
            drop(self.outbound.remove(id));
            return Err(ClientError::OutboundClosed);
        }
        guard.enqueued = true;

        // 5. Await the response.
        match rx.await.map_err(|_| ClientError::OutboundClosed)? {
            PendingOutcome::Response(Ok(bytes)) => {
                serde_json::from_slice::<R::Result>(&bytes).map_err(ClientError::Deserialize)
            }
            PendingOutcome::Response(Err(e)) => Err(ClientError::Remote(e)),
            PendingOutcome::Cancelled => Err(ClientError::Cancelled),
        }
    }

    /// Ask the client to display a document with `window/showDocument`
    /// (LSP 3.17), awaiting the client's [`ShowDocumentResult`].
    ///
    /// The [`ShowDocumentParams`] are sent exactly as provided: which document
    /// to show, whether it opens externally, and whether it takes focus all
    /// come from the caller. The helper owns no UI policy and the client's
    /// [`ShowDocumentResult`] — its success flag — is returned verbatim.
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`Client::request`]: [`ClientError::Serialize`],
    /// [`ClientError::ConnectionClosed`], [`ClientError::OutboundClosed`], or
    /// [`ClientError::IdExhausted`] if the request never reaches the wire;
    /// [`ClientError::Remote`] if the client answers with a JSON-RPC error;
    /// [`ClientError::Deserialize`] if the success result cannot be decoded;
    /// [`ClientError::Cancelled`] if the session closes before the client
    /// answers.
    pub async fn show_document(
        &self,
        params: ShowDocumentParams,
    ) -> Result<ShowDocumentResult, ClientError> {
        self.request::<ShowDocument>(params).await
    }

    /// Ask the user to pick one of several actions with
    /// `window/showMessageRequest` (LSP 3.17), awaiting the user's choice.
    ///
    /// The [`ShowMessageRequestParams`] are sent exactly as provided: the
    /// message, its type, and the action titles all come from the caller.
    /// The helper owns no message-selection policy: it never filters, ranks,
    /// or substitutes actions, and the client's `Option<MessageActionItem>` —
    /// the user's choice, or `None` on dismissal — is returned verbatim.
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`Client::request`]: [`ClientError::Serialize`],
    /// [`ClientError::ConnectionClosed`], [`ClientError::OutboundClosed`], or
    /// [`ClientError::IdExhausted`] if the request never reaches the wire;
    /// [`ClientError::Remote`] if the client answers with a JSON-RPC error;
    /// [`ClientError::Deserialize`] if the success result cannot be decoded;
    /// [`ClientError::Cancelled`] if the session closes before the client
    /// answers.
    pub async fn show_message_request(
        &self,
        params: ShowMessageRequestParams,
    ) -> Result<Option<MessageActionItem>, ClientError> {
        self.request::<ShowMessageRequest>(params).await
    }

    /// Ask the client to apply a workspace edit with `workspace/applyEdit`
    /// (LSP 3.17), awaiting the client's [`ApplyWorkspaceEditResponse`].
    ///
    /// The [`ApplyWorkspaceEditParams`] are sent exactly as provided: the
    /// edit contents, label, and metadata all come from the caller. The
    /// helper owns no edit policy: it never rewrites, filters, or batches
    /// edits, and the client's [`ApplyWorkspaceEditResponse`] — its `applied`
    /// flag with the optional failure reason — is returned verbatim for the
    /// caller to interpret.
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`Client::request`]: [`ClientError::Serialize`],
    /// [`ClientError::ConnectionClosed`], [`ClientError::OutboundClosed`], or
    /// [`ClientError::IdExhausted`] if the request never reaches the wire;
    /// [`ClientError::Remote`] if the client answers with a JSON-RPC error;
    /// [`ClientError::Deserialize`] if the success result cannot be decoded;
    /// [`ClientError::Cancelled`] if the session closes before the client
    /// answers.
    pub async fn apply_edit(
        &self,
        params: ApplyWorkspaceEditParams,
    ) -> Result<ApplyWorkspaceEditResponse, ClientError> {
        self.request::<ApplyWorkspaceEdit>(params).await
    }

    fn ensure_open(&self, phase: &mut Phase) -> Result<(), ClientError> {
        if self.outgoing.is_closed() {
            *phase = Phase::OutboundClosed;
            self.state.outbound_closing.cancel();
        }

        match phase {
            Phase::Open => Ok(()),
            Phase::ConnectionClosed => Err(ClientError::ConnectionClosed),
            Phase::OutboundClosed => Err(ClientError::OutboundClosed),
        }
    }

    pub(crate) fn close_connection(&self) {
        let mut phase = self.state.phase.lock().unwrap();
        if matches!(*phase, Phase::Open) {
            *phase = Phase::ConnectionClosed;
        }
    }

    pub(crate) fn close_outbound(&self) {
        let mut phase = self.state.phase.lock().unwrap();
        *phase = Phase::OutboundClosed;
        self.state.outbound_closing.cancel();
    }

    pub(crate) fn outbound_closing(&self) -> CancellationToken {
        self.state.outbound_closing.clone()
    }

    /// Engine-private accessor to the shared outbound registry.
    pub(crate) fn outbound_registry(&self) -> &OutboundRegistry {
        &self.outbound
    }

    /// The connection's shared trace level, handed to the
    /// [`Workspace`](crate::Workspace) when the engine establishes it, so
    /// `$/setTrace` writes and [`Client::log_trace`] reads observe one cell.
    pub(crate) fn shared_trace(&self) -> SharedTrace {
        self.trace.clone()
    }
}

#[cfg(test)]
mod tests {
    use lsp_types::NumberOrString;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::json;

    use super::*;

    enum TestNotification {}

    impl Notification for TestNotification {
        type Params = serde_json::Value;
        const METHOD: &'static str = "test/notification";
    }

    #[derive(Debug)]
    struct FailsToSerialize;

    impl Serialize for FailsToSerialize {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(serde::ser::Error::custom(
                "deliberate serialization failure",
            ))
        }
    }

    impl<'de> Deserialize<'de> for FailsToSerialize {
        fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            Ok(Self)
        }
    }

    enum FailingNotification {}

    impl Notification for FailingNotification {
        type Params = FailsToSerialize;
        const METHOD: &'static str = "test/fails-to-serialize";
    }

    fn make_client() -> (Client, tokio::sync::mpsc::UnboundedReceiver<RawMessage>) {
        let (outgoing, receiver) = tokio::sync::mpsc::unbounded_channel();
        let client = Client::new(outgoing, OutboundRegistry::default());
        (client, receiver)
    }

    #[test]
    fn serialization_failure_is_reported_without_enqueuing() {
        let (outgoing, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let client = Client::new(outgoing, OutboundRegistry::default());

        assert!(matches!(
            client.notify::<FailingNotification>(FailsToSerialize),
            Err(ClientError::Serialize(_))
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn closed_connection_is_reported_before_enqueue() {
        let (outgoing, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let client = Client::new(outgoing, OutboundRegistry::default());
        client.close_connection();

        assert!(matches!(
            client.notify::<TestNotification>(json!({ "value": 1 })),
            Err(ClientError::ConnectionClosed)
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn outbound_closure_rejects_every_new_notification() {
        let (outgoing, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let client = Client::new(outgoing, OutboundRegistry::default());
        client.close_connection();
        client.close_outbound();

        for value in [1, 2] {
            assert!(matches!(
                client.notify::<TestNotification>(json!({ "value": value })),
                Err(ClientError::OutboundClosed)
            ));
        }
        assert!(receiver.try_recv().is_err());
    }

    // --- OutboundRegistry unit tests -----------------------------------------

    #[test]
    fn outbound_ids_are_monotonic_and_never_reused() {
        let registry = OutboundRegistry::default();
        let (id1, _rx1) = insert_ok(&registry);
        let (id2, _rx2) = insert_ok(&registry);
        let (id3, _rx3) = insert_ok(&registry);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);

        // Completing an earlier request does not free its ID for reuse: the
        // allocator only ever moves forward.
        registry.complete(id1, Ok(Bytes::from_static(b"null")));
        let (id4, _rx4) = insert_ok(&registry);
        assert_eq!(id4, 4);
    }

    #[test]
    fn complete_returns_true_for_known_id_and_false_for_unknown() {
        let registry = OutboundRegistry::default();
        let (id, mut rx) = insert_ok(&registry);
        assert!(registry.complete(id, Ok(Bytes::from_static(b"null"))));
        // Now gone — second call returns false.
        assert!(!registry.complete(id, Ok(Bytes::from_static(b"null"))));
        // Unknown ID also returns false.
        assert!(!registry.complete(999, Ok(Bytes::from_static(b"null"))));
        // The receiver should have received the value.
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn close_all_completes_pending_with_cancelled() {
        let registry = OutboundRegistry::default();
        let (_id1, mut rx1) = insert_ok(&registry);
        let (_id2, mut rx2) = insert_ok(&registry);
        registry.close_all();
        assert!(matches!(rx1.try_recv().unwrap(), PendingOutcome::Cancelled));
        assert!(matches!(rx2.try_recv().unwrap(), PendingOutcome::Cancelled));
        // Registry is drained; nothing leaks.
        assert_eq!(registry.pending_len(), 0);
    }

    #[test]
    fn outbound_id_space_exhaustion_returns_none() {
        let registry = OutboundRegistry::default();
        // Force the allocator to the last usable positive ID.
        registry.set_next_id(i32::MAX as u32);
        let (id, _rx) = insert_ok(&registry);
        assert_eq!(id, i32::MAX as u32);
        // The next allocation is refused: the positive ID space is exhausted.
        assert!(matches!(registry.insert(), InsertOutcome::Exhausted));
    }

    #[test]
    fn insert_after_close_all_is_refused() {
        let registry = OutboundRegistry::default();
        registry.close_all();
        // A handler that starts a new outbound request after the registry has
        // already been drained must fail fast instead of enqueuing a request
        // that would never be completed.
        assert!(matches!(registry.insert(), InsertOutcome::Closed));
    }

    /// Test helper: unwrap an `InsertOutcome::Inserted`, panicking otherwise.
    fn insert_ok(registry: &OutboundRegistry) -> (u32, oneshot::Receiver<PendingOutcome>) {
        match registry.insert() {
            InsertOutcome::Inserted(id, rx) => (id, rx),
            InsertOutcome::Closed => panic!("registry unexpectedly closed"),
            InsertOutcome::Exhausted => panic!("registry unexpectedly exhausted"),
        }
    }

    #[tokio::test]
    async fn request_completes_when_response_arrives() {
        use lsp_types::request::Request as LspRequest;

        enum PingRequest {}
        impl LspRequest for PingRequest {
            type Params = serde_json::Value;
            type Result = String;
            const METHOD: &'static str = "test/ping";
        }

        let (client, mut receiver) = make_client();
        let client2 = client.clone();

        let handle = tokio::spawn(async move { client2.request::<PingRequest>(json!({})).await });

        // Pull the request off the wire.
        let msg = receiver.recv().await.unwrap();
        let id = match &msg {
            RawMessage::Request { id, .. } => id.clone(),
            _ => panic!("expected a request"),
        };

        // Deliver a response.
        let id_num = match &id {
            lsp_types::NumberOrString::Number(n) => *n as u32,
            _ => panic!("expected numeric id"),
        };
        client
            .outbound_registry()
            .complete(id_num, Ok(Bytes::from(b"\"pong\"".to_vec())));

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result, "pong");
    }

    #[tokio::test]
    async fn request_returns_remote_error_on_error_response() {
        use lsp_types::request::Request as LspRequest;

        enum PingRequest {}
        impl LspRequest for PingRequest {
            type Params = serde_json::Value;
            type Result = String;
            const METHOD: &'static str = "test/ping";
        }

        let (client, mut receiver) = make_client();
        let client2 = client.clone();

        let handle = tokio::spawn(async move { client2.request::<PingRequest>(json!({})).await });

        let msg = receiver.recv().await.unwrap();
        let id_num = match msg {
            RawMessage::Request {
                id: NumberOrString::Number(n),
                ..
            } => n as u32,
            _ => panic!("expected numeric request"),
        };

        client.outbound_registry().complete(
            id_num,
            Err(JsonRpcError {
                code: -32000,
                message: "server error".to_string(),
                data: Some(json!({ "retry": true })),
            }),
        );

        // The remote error's code, message, and optional data are preserved.
        let err = handle.await.unwrap().unwrap_err();
        match err {
            ClientError::Remote(e) => {
                assert_eq!(e.code, -32000);
                assert_eq!(e.message, "server error");
                assert_eq!(e.data, Some(json!({ "retry": true })));
            }
            other => panic!("expected ClientError::Remote, got {other:?}"),
        }
        assert_eq!(client.outbound_registry().pending_len(), 0);
    }

    #[tokio::test]
    async fn malformed_success_result_returns_deserialize_error() {
        use lsp_types::request::Request as LspRequest;

        enum TypedRequest {}
        impl LspRequest for TypedRequest {
            type Params = serde_json::Value;
            type Result = u32;
            const METHOD: &'static str = "test/typed";
        }

        let (client, mut receiver) = make_client();
        let client2 = client.clone();

        let handle = tokio::spawn(async move { client2.request::<TypedRequest>(json!({})).await });

        let msg = receiver.recv().await.unwrap();
        let id_num = match msg {
            RawMessage::Request {
                id: NumberOrString::Number(n),
                ..
            } => n as u32,
            _ => panic!("expected numeric request"),
        };

        // A success envelope whose result does not decode into `R::Result`.
        client
            .outbound_registry()
            .complete(id_num, Ok(Bytes::from_static(b"\"not a number\"")));

        let err = handle.await.unwrap().unwrap_err();
        assert!(
            matches!(err, ClientError::Deserialize(_)),
            "expected ClientError::Deserialize, got {err:?}"
        );
        assert_eq!(client.outbound_registry().pending_len(), 0);
    }

    #[tokio::test]
    async fn dropping_request_future_removes_pending_entry() {
        use lsp_types::request::Request as LspRequest;

        enum PingRequest {}
        impl LspRequest for PingRequest {
            type Params = serde_json::Value;
            type Result = String;
            const METHOD: &'static str = "test/ping";
        }

        let (client, mut receiver) = make_client();
        let client2 = client.clone();

        // Spawn the request future, then immediately drop the JoinHandle
        // which cancels the task before the response arrives.
        let handle = tokio::spawn(async move { client2.request::<PingRequest>(json!({})).await });

        // Pull the request off the wire so we know insert() ran.
        let msg = receiver.recv().await.unwrap();
        let id_num = match msg {
            RawMessage::Request {
                id: NumberOrString::Number(n),
                ..
            } => n as u32,
            _ => panic!("expected numeric request"),
        };

        // Abort the task (simulates caller cancellation).
        handle.abort();
        let _ = handle.await; // wait for abort to complete

        // The pending entry must have been removed by PendingGuard.
        // complete() should return false because the entry is gone.
        assert!(
            !client
                .outbound_registry()
                .complete(id_num, Ok(Bytes::from_static(b"\"pong\""))),
            "pending entry was not cleaned up after future was dropped"
        );

        // Dropping the future also emits one typed $/cancelRequest for the ID.
        let cancel = receiver.recv().await.unwrap();
        match cancel {
            RawMessage::Notification { method, params } => {
                assert_eq!(method, "$/cancelRequest");
                let params: serde_json::Value = serde_json::from_slice(&params).unwrap();
                assert_eq!(params["id"], serde_json::json!(id_num));
            }
            _ => panic!("expected a $/cancelRequest notification"),
        }
        // Exactly one cancellation notification; nothing else leaks.
        assert!(receiver.try_recv().is_err());
        assert_eq!(client.outbound_registry().pending_len(), 0);
    }

    #[tokio::test]
    async fn abandoned_enqueued_request_emits_one_cancel_notification() {
        use lsp_types::request::Request as LspRequest;

        enum PingRequest {}
        impl LspRequest for PingRequest {
            type Params = serde_json::Value;
            type Result = String;
            const METHOD: &'static str = "test/ping";
        }

        let (client, mut receiver) = make_client();
        let client2 = client.clone();

        let handle = tokio::spawn(async move { client2.request::<PingRequest>(json!({})).await });

        // Pull the request off the wire; its ID must be echoed back.
        let msg = receiver.recv().await.unwrap();
        let id_num = match msg {
            RawMessage::Request {
                id: NumberOrString::Number(n),
                ..
            } => n,
            _ => panic!("expected numeric request"),
        };

        // Abort the caller; the guard drops and must emit a typed cancel.
        handle.abort();
        let _ = handle.await;

        let cancel = receiver.recv().await.unwrap();
        match cancel {
            RawMessage::Notification { method, params } => {
                assert_eq!(method, "$/cancelRequest");
                let params: serde_json::Value = serde_json::from_slice(&params).unwrap();
                assert_eq!(params["id"], serde_json::json!(id_num));
            }
            _ => panic!("expected a $/cancelRequest notification"),
        }

        // Exactly one notification; nothing else leaks.
        assert!(receiver.try_recv().is_err());
        assert_eq!(client.outbound_registry().pending_len(), 0);
    }

    #[tokio::test]
    async fn close_all_yields_cancelled_error_from_request() {
        use lsp_types::request::Request as LspRequest;

        enum PingRequest {}
        impl LspRequest for PingRequest {
            type Params = serde_json::Value;
            type Result = String;
            const METHOD: &'static str = "test/ping";
        }

        let (client, mut receiver) = make_client();
        let client2 = client.clone();

        let handle = tokio::spawn(async move { client2.request::<PingRequest>(json!({})).await });

        // Pull the request off the wire so we know it was enqueued.
        let msg = receiver.recv().await.unwrap();
        assert!(matches!(msg, RawMessage::Request { .. }));

        // Session closes: every pending entry completes with Cancelled.
        client.outbound_registry().close_all();

        let err = handle.await.unwrap().unwrap_err();
        assert!(
            matches!(err, ClientError::Cancelled),
            "expected Cancelled, got {err:?}"
        );
        assert_eq!(client.outbound_registry().pending_len(), 0);
    }

    #[tokio::test]
    async fn stale_response_after_cleanup_cannot_complete_another_request() {
        use lsp_types::request::Request as LspRequest;

        enum PingRequest {}
        impl LspRequest for PingRequest {
            type Params = serde_json::Value;
            type Result = String;
            const METHOD: &'static str = "test/ping";
        }

        let (client, mut receiver) = make_client();
        let client2 = client.clone();

        let handle = tokio::spawn(async move { client2.request::<PingRequest>(json!({})).await });

        // Pull request A off the wire.
        let msg = receiver.recv().await.unwrap();
        let id_a = match msg {
            RawMessage::Request {
                id: NumberOrString::Number(n),
                ..
            } => n as u32,
            _ => panic!("expected numeric request"),
        };

        // Abandon request A: the guard removes its entry and emits a cancel.
        handle.abort();
        let _ = handle.await;
        let cancel = receiver.recv().await.unwrap();
        assert!(matches!(
            cancel,
            RawMessage::Notification { method, .. } if &*method == "$/cancelRequest"
        ));

        // Request B gets a fresh, never-reused ID.
        let client3 = client.clone();
        let handle_b = tokio::spawn(async move { client3.request::<PingRequest>(json!({})).await });
        let msg = receiver.recv().await.unwrap();
        let id_b = match msg {
            RawMessage::Request {
                id: NumberOrString::Number(n),
                ..
            } => n as u32,
            _ => panic!("expected numeric request"),
        };
        assert_ne!(id_a, id_b, "abandoned ID must never be reused");

        // A stale response for A cannot complete B.
        client
            .outbound_registry()
            .complete(id_a, Ok(Bytes::from_static(b"\"stale\"")));
        assert_eq!(client.outbound_registry().pending_len(), 1);

        // The real response for B completes B with its own result.
        client
            .outbound_registry()
            .complete(id_b, Ok(Bytes::from_static(b"\"pong\"")));
        let result = handle_b.await.unwrap().unwrap();
        assert_eq!(result, "pong");
        assert_eq!(client.outbound_registry().pending_len(), 0);
    }

    #[tokio::test]
    async fn enqueue_failure_emits_no_cancel_and_leaves_no_entry() {
        use lsp_types::request::Request as LspRequest;

        enum PingRequest {}
        impl LspRequest for PingRequest {
            type Params = serde_json::Value;
            type Result = String;
            const METHOD: &'static str = "test/ping";
        }

        // A client whose outbound queue is closed (receiver dropped, so all
        // sends fail).
        let (outgoing, receiver) = tokio::sync::mpsc::unbounded_channel();
        let client = Client::new(outgoing, OutboundRegistry::default());
        drop(receiver);
        let client2 = client.clone();

        let result = client2.request::<PingRequest>(json!({})).await;
        assert!(matches!(result, Err(ClientError::OutboundClosed)));

        // Nothing was ever emitted, and the registry is empty.
        assert_eq!(client.outbound_registry().pending_len(), 0);
    }
}
