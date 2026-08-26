use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures_channel::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    oneshot,
};
use lsp_types::notification::{
    LogMessage, LogTrace, Notification, Progress, PublishDiagnostics, ShowMessage,
};
use lsp_types::request::{
    ApplyWorkspaceEdit, CodeLensRefresh, InlayHintRefreshRequest, InlineValueRefreshRequest,
    RegisterCapability, Request, SemanticTokensRefresh, ShowDocument, ShowMessageRequest,
    UnregisterCapability, WorkspaceConfiguration, WorkspaceDiagnosticRefresh,
    WorkspaceFoldersRequest,
};
use lsp_types::{
    ApplyWorkspaceEditParams, ApplyWorkspaceEditResponse, ConfigurationParams, LogMessageParams,
    LogTraceParams, MessageActionItem, ProgressParams, PublishDiagnosticsParams,
    RegistrationParams, ShowDocumentParams, ShowDocumentResult, ShowMessageParams,
    ShowMessageRequestParams, TraceValue, UnregistrationParams, WorkspaceFolder,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{trace, warn};

use crate::error::ClientError;
use crate::progress::ProgressRegistry;
use crate::raw::{JsonRpcError, RawMessage, RequestId};
use crate::telemetry::{Completion, Deadline, DeadlineAction, Direction, Resource, ResourceAction};
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
/// `ClientHandle` handles (which await their specific entry).
#[derive(Clone, Default)]
pub(crate) struct OutboundRegistry {
    inner: Arc<Mutex<OutboundInner>>,
}

struct OutboundInner {
    /// Pending requests keyed by their outbound ID.
    pending: HashMap<u32, PendingEntry>,
    /// Monotonically increasing counter; the next ID to try allocating.
    next_id: u32,
    /// Set by `close_all()`. Once set, `insert()` refuses to create new
    /// pending entries: any handler that starts a new outbound request after
    /// the registry has been drained fails fast instead of enqueuing an entry
    /// that would never be completed.
    closed: bool,
}

struct PendingEntry {
    tx: oneshot::Sender<PendingOutcome>,
    telemetry: Option<PendingTelemetry>,
    deadline_started: Option<crate::telemetry::Instant>,
}

#[derive(Clone, Copy)]
struct PendingTelemetry {
    trace: crate::telemetry::ConnectionTrace,
    method: &'static str,
    timeout: Option<Duration>,
    started: crate::telemetry::Instant,
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
    client: ClientHandle,
    id: u32,
    enqueued: bool,
}

impl PendingGuard {
    fn new(client: ClientHandle, id: u32) -> Self {
        Self {
            client,
            id,
            enqueued: false,
        }
    }

    /// Claim an unanswered request and notify the peer that it was cancelled.
    ///
    /// The registry removal is the completion race: if it succeeds, this
    /// guard owns expiry or abandonment; if it fails, a response or connection
    /// close already owns completion and the receiver must be awaited.
    fn cancel_if_pending(
        &mut self,
        completion: Completion,
        deadline_action: DeadlineAction,
    ) -> bool {
        if self
            .client
            .outbound
            .remove_with_completion(self.id, completion, deadline_action)
            .is_none()
        {
            return false;
        }
        if self.enqueued {
            let _ = self
                .client
                .notify_required::<lsp_types::notification::Cancel>(lsp_types::CancelParams {
                    id: lsp_types::NumberOrString::Number(self.id as i32),
                });
        }
        self.enqueued = false;
        true
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        // Only a still-present entry means the request never completed. Remove
        // it and, if it reached the wire, cancel it with the peer. Errors are
        // ignored: the connection may already be closing.
        self.cancel_if_pending(Completion::Cancelled, DeadlineAction::Cancelled);
    }
}

#[cfg(any(
    all(not(target_arch = "wasm32"), feature = "runtime-tokio"),
    target_arch = "wasm32",
))]
async fn wait_for_response_until(
    receiver: &mut oneshot::Receiver<PendingOutcome>,
    timeout: Duration,
) -> Option<Result<PendingOutcome, oneshot::Canceled>> {
    use futures_util::{FutureExt, pin_mut, select_biased};

    let response = receiver.fuse();
    let deadline = crate::runtime::sleep(timeout).fuse();
    pin_mut!(response, deadline);

    select_biased! {
        outcome = response => Some(outcome),
        () = deadline => None,
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
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn insert(&self) -> InsertOutcome {
        self.insert_inner(None)
    }

    fn insert_traced(
        &self,
        trace: crate::telemetry::ConnectionTrace,
        method: &'static str,
        timeout: Option<Duration>,
    ) -> InsertOutcome {
        self.insert_inner(Some((trace, method, timeout)))
    }

    fn insert_inner(
        &self,
        telemetry: Option<(
            crate::telemetry::ConnectionTrace,
            &'static str,
            Option<Duration>,
        )>,
    ) -> InsertOutcome {
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
        let telemetry = telemetry.map(|(trace, method, timeout)| PendingTelemetry {
            trace,
            method,
            timeout,
            started: crate::telemetry::Instant::now(),
        });
        inner.pending.insert(
            id,
            PendingEntry {
                tx,
                telemetry,
                deadline_started: None,
            },
        );
        if let Some(telemetry) = telemetry {
            telemetry.trace.pending_request_budget(
                telemetry.method,
                ResourceAction::Admit,
                id,
                inner.pending.len(),
                telemetry.timeout,
            );
        }
        InsertOutcome::Inserted(id, rx)
    }

    fn mark_enqueued(&self, id: u32) {
        let mut inner = self.inner.lock().unwrap();
        let Some(entry) = inner.pending.get_mut(&id) else {
            return;
        };
        let Some(telemetry) = entry.telemetry else {
            return;
        };
        let started = crate::telemetry::Instant::now();
        entry.deadline_started = Some(started);
        if let Some(timeout) = telemetry.timeout {
            telemetry.trace.deadline(
                Deadline::OutboundRequest,
                DeadlineAction::Armed,
                Direction::Outbound,
                telemetry.method,
                &RequestId::Number(id as i32),
                timeout,
                Duration::ZERO,
            );
        }
    }

    /// Remove and complete the pending entry for `id` with `result`.
    ///
    /// If no entry exists (unknown, duplicate, or late response), returns
    /// `false` and leaves all other entries intact.
    pub(crate) fn complete(&self, id: u32, result: PendingResult) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.pending.remove(&id);
        if let Some(entry) = entry {
            trace_pending_removal(
                &entry,
                id,
                inner.pending.len(),
                ResourceAction::Release,
                if result.is_ok() {
                    Completion::Success
                } else {
                    Completion::Error
                },
                DeadlineAction::Completed,
            );
            drop(inner);
            // Receiver may be gone if the caller was cancelled; ignore.
            let _ = entry.tx.send(PendingOutcome::Response(result));
            true
        } else {
            false
        }
    }

    /// Roll back the pending entry for `id` and trace its rejected completion.
    ///
    /// Used when encoding or enqueue fails immediately after `insert`, and by
    /// the pending guard on abandonment.
    fn remove(&self, id: u32) -> Option<PendingEntry> {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.pending.remove(&id);
        if let Some(entry) = &entry {
            trace_pending_removal(
                entry,
                id,
                inner.pending.len(),
                ResourceAction::Rollback,
                Completion::Rejected,
                DeadlineAction::Cancelled,
            );
        }
        entry
    }

    fn remove_with_completion(
        &self,
        id: u32,
        completion: Completion,
        deadline_action: DeadlineAction,
    ) -> Option<PendingEntry> {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.pending.remove(&id);
        if let Some(entry) = &entry {
            trace_pending_removal(
                entry,
                id,
                inner.pending.len(),
                ResourceAction::Release,
                completion,
                deadline_action,
            );
        }
        entry
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
        let entries: HashMap<u32, PendingEntry> = std::mem::take(&mut inner.pending);
        let mut remaining = entries.len();
        for (id, entry) in &entries {
            remaining -= 1;
            trace_pending_removal(
                entry,
                *id,
                remaining,
                ResourceAction::Release,
                Completion::ConnectionClosed,
                DeadlineAction::Cancelled,
            );
        }
        drop(inner);
        for entry in entries.into_values() {
            let _ = entry.tx.send(PendingOutcome::Cancelled);
        }
    }

    /// Override the next candidate ID (test-only, for exhaustion coverage).
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn set_next_id(&self, id: u32) {
        self.inner.lock().unwrap().next_id = id;
    }

    /// Number of entries currently pending.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn pending_len(&self) -> usize {
        self.inner.lock().unwrap().pending.len()
    }
}

fn trace_pending_removal(
    entry: &PendingEntry,
    id: u32,
    current: usize,
    action: ResourceAction,
    completion: Completion,
    deadline_action: DeadlineAction,
) {
    let Some(telemetry) = entry.telemetry else {
        return;
    };
    telemetry.trace.pending_request_budget(
        telemetry.method,
        action,
        id,
        current,
        telemetry.timeout,
    );
    let request_id = RequestId::Number(id as i32);
    telemetry.trace.request_completed(
        telemetry.method,
        &request_id,
        telemetry.started,
        Direction::Outbound,
        completion,
    );
    if let (Some(timeout), Some(started)) = (telemetry.timeout, entry.deadline_started) {
        telemetry.trace.deadline(
            Deadline::OutboundRequest,
            deadline_action,
            Direction::Outbound,
            telemetry.method,
            &request_id,
            timeout,
            if matches!(deadline_action, DeadlineAction::Expired) {
                timeout
            } else {
                started.elapsed()
            },
        );
    }
}

/// The engine-owned outbound queue for one connection. Admission bounds both
/// retained message count and exact encoded JSON-RPC envelope bytes while an
/// unbounded channel remains the runtime-neutral wake-up primitive. Every
/// successful [`send`](Self::send) charges both budgets; the send-loop calls
/// [`record_done`](Self::record_done) after each transport attempt.
#[derive(Clone)]
pub(crate) struct OutboundQueue {
    tx: UnboundedSender<RawMessage>,
    inner: Arc<OutboundAdmission>,
}

struct OutboundAdmission {
    threshold: usize,
    limits: OutboundLimits,
    failure: CancellationToken,
    trace: crate::telemetry::ConnectionTrace,
    failure_reporter: crate::failure::FailureReporter,
    /// Depth and warn-state move together under one lock, so a concurrent
    /// enqueue can never observe a stale warn-state for a depth that has
    /// already dropped back below the threshold (and vice versa).
    state: Mutex<AdmissionState>,
}

#[derive(Clone, Copy)]
struct OutboundLimits {
    max_messages: usize,
    max_bytes: usize,
}

struct AdmissionState {
    depth: usize,
    encoded_bytes: usize,
    encoded_sizes: VecDeque<usize>,
    /// Set once depth reaches the threshold and cleared once it drops back
    /// below it, so one sustained crossing produces exactly one warning.
    warned: bool,
}

#[derive(Debug)]
pub(crate) enum OutboundSendError {
    Overloaded,
    Closed,
    Serialize(serde_json::Error),
}

#[derive(Clone, Copy)]
enum DeliveryRequirement {
    Ordinary,
    Required,
}

impl OutboundQueue {
    /// Create the queue with its receiving half. `threshold` is the depth at
    /// which one warning is emitted per upward crossing.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn new(threshold: usize) -> (Self, UnboundedReceiver<RawMessage>) {
        let trace = crate::telemetry::ConnectionTrace::new();
        Self::with_limits(
            threshold,
            OutboundLimits {
                max_messages: usize::MAX,
                max_bytes: usize::MAX,
            },
            trace,
            crate::failure::FailureReporter::new(None, trace.id()),
        )
    }

    #[cfg(test)]
    pub(crate) fn bounded(
        max_messages: usize,
        max_bytes: usize,
    ) -> (Self, UnboundedReceiver<RawMessage>) {
        Self::bounded_with_trace(
            max_messages,
            max_bytes,
            crate::telemetry::ConnectionTrace::new(),
        )
    }

    #[cfg(test)]
    pub(crate) fn bounded_with_trace(
        max_messages: usize,
        max_bytes: usize,
        trace: crate::telemetry::ConnectionTrace,
    ) -> (Self, UnboundedReceiver<RawMessage>) {
        Self::bounded_with_reporter(
            max_messages,
            max_bytes,
            trace,
            crate::failure::FailureReporter::new(None, trace.id()),
        )
    }

    pub(crate) fn bounded_with_reporter(
        max_messages: usize,
        max_bytes: usize,
        trace: crate::telemetry::ConnectionTrace,
        failure_reporter: crate::failure::FailureReporter,
    ) -> (Self, UnboundedReceiver<RawMessage>) {
        Self::with_limits(
            max_messages,
            OutboundLimits {
                max_messages,
                max_bytes,
            },
            trace,
            failure_reporter,
        )
    }

    fn with_limits(
        threshold: usize,
        limits: OutboundLimits,
        trace: crate::telemetry::ConnectionTrace,
        failure_reporter: crate::failure::FailureReporter,
    ) -> (Self, UnboundedReceiver<RawMessage>) {
        let (tx, rx) = mpsc::unbounded();
        (
            Self {
                tx,
                inner: Arc::new(OutboundAdmission {
                    threshold,
                    limits,
                    failure: CancellationToken::new(),
                    trace,
                    failure_reporter,
                    state: Mutex::new(AdmissionState {
                        depth: 0,
                        encoded_bytes: 0,
                        encoded_sizes: VecDeque::new(),
                        warned: false,
                    }),
                }),
            },
            rx,
        )
    }

    /// Enqueue one message. The depth is counted before the send so the
    /// writer can never decrement a message that is not yet counted; a failed
    /// send means the writer half is gone, so the message is uncounted again.
    pub(crate) fn send(&self, message: RawMessage) -> Result<(), OutboundSendError> {
        self.send_with_admission(message, || {})
    }

    fn send_with_admission(
        &self,
        message: RawMessage,
        on_admitted: impl FnOnce(),
    ) -> Result<(), OutboundSendError> {
        let encoded_bytes = crate::transport::envelope::serialize(&message)
            .map_err(|error| match error {
                crate::TransportError::Serde(error) => OutboundSendError::Serialize(error),
                other => unreachable!("envelope serialization cannot produce {other}"),
            })?
            .len();
        let mut state = self.inner.state.lock().unwrap();
        if state.depth >= self.inner.limits.max_messages
            || state
                .encoded_bytes
                .checked_add(encoded_bytes)
                .is_none_or(|bytes| bytes > self.inner.limits.max_bytes)
        {
            let (method, request_id) = match &message {
                RawMessage::Request { method, id, .. } => (Some(method.as_ref()), Some(id)),
                RawMessage::Notification { method, .. } => (Some(method.as_ref()), None),
                RawMessage::Response { id, .. } => (None, Some(id)),
                RawMessage::ProtocolError { .. } => (None, None),
            };
            self.inner.trace.resource_budget(
                Resource::OutboundQueue,
                ResourceAction::Reject,
                state.depth,
                self.inner.limits.max_messages,
                Some((state.encoded_bytes, self.inner.limits.max_bytes)),
            );
            warn!(
                outbound.queue_depth = state.depth,
                outbound.queue_bytes = state.encoded_bytes,
                max_messages = self.inner.limits.max_messages,
                max_bytes = self.inner.limits.max_bytes,
                "outbound queue capacity is exhausted"
            );
            drop(state);
            self.inner.failure_reporter.report(
                crate::ConnectionFailureCategory::Overload,
                Some(crate::ConnectionDirection::Outbound),
                method,
                request_id,
            );
            return Err(OutboundSendError::Overloaded);
        }
        self.record_enqueue(&mut state, encoded_bytes);
        on_admitted();
        if self.tx.unbounded_send(message).is_err() {
            self.record_failed_enqueue(&mut state);
            return Err(OutboundSendError::Closed);
        }
        Ok(())
    }

    /// Enqueue protocol traffic that must not be silently lost. If it cannot
    /// be retained within the same finite budgets as ordinary traffic, signal
    /// the engine's single failure-close path.
    pub(crate) fn send_required(&self, message: RawMessage) -> Result<(), OutboundSendError> {
        self.send(message)
            .inspect_err(|_| self.inner.failure.cancel())
    }

    pub(crate) fn failure(&self) -> CancellationToken {
        self.inner.failure.clone()
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    pub(crate) fn connection_trace(&self) -> crate::telemetry::ConnectionTrace {
        self.inner.trace
    }

    pub(crate) fn failure_reporter(&self) -> crate::failure::FailureReporter {
        self.inner.failure_reporter.clone()
    }

    /// The current queue depth.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn depth(&self) -> usize {
        self.inner.state.lock().unwrap().depth
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn encoded_bytes(&self) -> usize {
        self.inner.state.lock().unwrap().encoded_bytes
    }

    /// Count one enqueued request, response, or notification, warning once
    /// when the depth crosses from below the threshold to at least it.
    ///
    /// Stable resource events record every accounting transition without
    /// including the queued envelope or its parameters.
    fn record_enqueue(&self, state: &mut AdmissionState, encoded_bytes: usize) {
        state.depth += 1;
        state.encoded_bytes += encoded_bytes;
        state.encoded_sizes.push_back(encoded_bytes);
        let depth = state.depth;
        self.inner.trace.resource_budget(
            Resource::OutboundQueue,
            ResourceAction::Admit,
            depth,
            self.inner.limits.max_messages,
            Some((state.encoded_bytes, self.inner.limits.max_bytes)),
        );
        if depth >= self.inner.threshold {
            trace!(outbound.queue_depth = depth, "outbound message enqueued");
            if !state.warned {
                state.warned = true;
                warn!(
                    outbound.queue_depth = depth,
                    threshold = self.inner.threshold,
                    "outbound queue depth reached the warning threshold"
                );
            }
        }
    }

    /// Count one message the queue is done with: the writer calls this after
    /// each transport send, whether it succeeded or failed, and `send` calls
    /// it to uncount a message whose channel enqueue failed. Dropping back
    /// below the threshold rearms one warning for the next crossing.
    pub(crate) fn record_done(&self) {
        let mut state = self.inner.state.lock().unwrap();
        let encoded_bytes = state
            .encoded_sizes
            .pop_front()
            .expect("only an accounted outbound message can finish");
        state.depth -= 1;
        state.encoded_bytes -= encoded_bytes;
        let depth = state.depth;
        self.inner.trace.resource_budget(
            Resource::OutboundQueue,
            ResourceAction::Release,
            depth,
            self.inner.limits.max_messages,
            Some((state.encoded_bytes, self.inner.limits.max_bytes)),
        );
        if depth >= self.inner.threshold {
            trace!(outbound.queue_depth = depth, "outbound message written");
        } else {
            state.warned = false;
        }
    }

    fn record_failed_enqueue(&self, state: &mut AdmissionState) {
        let encoded_bytes = state
            .encoded_sizes
            .pop_back()
            .expect("the failed channel send was accounted first");
        state.depth -= 1;
        state.encoded_bytes -= encoded_bytes;
        self.inner.trace.resource_budget(
            Resource::OutboundQueue,
            ResourceAction::Rollback,
            state.depth,
            self.inner.limits.max_messages,
            Some((state.encoded_bytes, self.inner.limits.max_bytes)),
        );
        if state.depth < self.inner.threshold {
            state.warned = false;
        }
    }

    pub(crate) fn discard_all(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.depth = 0;
        state.encoded_bytes = 0;
        state.encoded_sizes.clear();
        state.warned = false;
        self.inner.trace.resource_budget(
            Resource::OutboundQueue,
            ResourceAction::Release,
            0,
            self.inner.limits.max_messages,
            Some((0, self.inner.limits.max_bytes)),
        );
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
/// A `ClientHandle` is connection-scoped. It does not expose the connection's
/// outbound queue or protocol registries; cloning it only clones a cheap
/// handle into facilities owned by the protocol engine.
#[derive(Clone)]
pub struct ClientHandle {
    outgoing: OutboundQueue,
    outbound: OutboundRegistry,
    outbound_request_timeout: Option<Duration>,
    progress: ProgressRegistry,
    state: Arc<ClientState>,
    trace: SharedTrace,
    telemetry: crate::telemetry::ConnectionTrace,
    failure_reporter: crate::failure::FailureReporter,
}

impl fmt::Debug for ClientHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientHandle")
            .finish_non_exhaustive()
    }
}

impl ClientHandle {
    pub(crate) fn new(
        outgoing: OutboundQueue,
        outbound: OutboundRegistry,
        outbound_request_timeout: Option<Duration>,
    ) -> Self {
        let telemetry = outgoing.connection_trace();
        let failure_reporter = outgoing.failure_reporter();
        Self {
            outgoing,
            outbound,
            outbound_request_timeout,
            progress: ProgressRegistry::default(),
            state: Arc::new(ClientState {
                phase: Mutex::new(Phase::Open),
                outbound_closing: CancellationToken::new(),
            }),
            trace: SharedTrace::default(),
            telemetry,
            failure_reporter,
        }
    }

    /// Encode and enqueue one typed server-to-client notification.
    ///
    /// Notifications are fire-and-forget: this method returns synchronously,
    /// allocates no request ID, and creates no pending request entry. If the
    /// outbound message or byte budget is full, it returns
    /// [`ClientError::OutboundOverloaded`] without retaining the notification.
    pub fn notify<N>(&self, params: N::Params) -> Result<(), ClientError>
    where
        N: Notification,
    {
        self.notify_with_requirement::<N>(params, DeliveryRequirement::Ordinary)
    }

    pub(crate) fn notify_required<N>(&self, params: N::Params) -> Result<(), ClientError>
    where
        N: Notification,
    {
        self.notify_with_requirement::<N>(params, DeliveryRequirement::Required)
    }

    fn notify_with_requirement<N>(
        &self,
        params: N::Params,
        requirement: DeliveryRequirement,
    ) -> Result<(), ClientError>
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
        let sent = match requirement {
            DeliveryRequirement::Ordinary => self.outgoing.send(message),
            DeliveryRequirement::Required => self.outgoing.send_required(message),
        };
        match sent {
            Ok(()) => {}
            Err(OutboundSendError::Overloaded) => {
                return Err(ClientError::OutboundOverloaded);
            }
            Err(OutboundSendError::Closed) => {
                *phase = Phase::OutboundClosed;
                self.state.outbound_closing.cancel();
                return Err(ClientError::OutboundClosed);
            }
            Err(OutboundSendError::Serialize(error)) => {
                return Err(ClientError::Serialize(error));
            }
        }
        Ok(())
    }

    /// The shared enqueue path of the named outgoing helpers: any
    /// serialization or enqueue failure is reported through `tracing` and
    /// still returned to the caller.
    pub(crate) fn notify_logged<N>(&self, params: N::Params) -> Result<(), ClientError>
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
    /// - [`ClientError::OutboundOverloaded`] if the request would exceed the
    ///   connection's queued-message or encoded-byte budget. Its pending
    ///   registry entry is removed before this error is returned.
    /// - [`ClientError::IdExhausted`] if the outbound ID space is exhausted.
    /// - [`ClientError::Serialize`] if the params cannot be encoded.
    /// - [`ClientError::Cancelled`] if the session closes before the peer
    ///   answers.
    /// - [`ClientError::Timeout`] if the configured outbound-request deadline
    ///   expires before the peer answers.
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
        let (id, rx) = match self.outbound.insert_traced(
            self.telemetry,
            R::METHOD,
            self.outbound_request_timeout,
        ) {
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
                Ok(()) => match self
                    .outgoing
                    .send_with_admission(message, || self.outbound.mark_enqueued(id))
                {
                    Ok(()) => true,
                    Err(OutboundSendError::Overloaded) => {
                        drop(self.outbound.remove(id));
                        return Err(ClientError::OutboundOverloaded);
                    }
                    Err(OutboundSendError::Closed) => {
                        *phase = Phase::OutboundClosed;
                        self.state.outbound_closing.cancel();
                        false
                    }
                    Err(OutboundSendError::Serialize(error)) => {
                        drop(self.outbound.remove(id));
                        return Err(ClientError::Serialize(error));
                    }
                },
            }
        };

        if !enqueued {
            // Outbound closed during send: remove pending, report error.
            drop(self.outbound.remove(id));
            return Err(ClientError::OutboundClosed);
        }
        guard.enqueued = true;

        // 5. Await the response, bounded by the connection policy unless the
        //    deadline was explicitly disabled. Expiry atomically removes the
        //    registry entry before reporting Timeout; if response completion
        //    won that race, await and return that already-owned outcome.
        #[cfg(any(
            all(not(target_arch = "wasm32"), feature = "runtime-tokio"),
            target_arch = "wasm32",
        ))]
        let mut rx = rx;
        let outcome = match self.outbound_request_timeout {
            #[cfg(any(
                all(not(target_arch = "wasm32"), feature = "runtime-tokio"),
                target_arch = "wasm32",
            ))]
            Some(timeout) => match wait_for_response_until(&mut rx, timeout).await {
                Some(outcome) => outcome,
                None if guard
                    .cancel_if_pending(Completion::DeadlineExpired, DeadlineAction::Expired) =>
                {
                    return Err(ClientError::Timeout);
                }
                None => rx.await,
            },
            #[cfg(all(not(target_arch = "wasm32"), not(feature = "runtime-tokio")))]
            Some(_) => rx.await,
            None => rx.await,
        }
        .map_err(|_| ClientError::OutboundClosed)?;

        match outcome {
            PendingOutcome::Response(Ok(bytes)) => serde_json::from_slice::<R::Result>(&bytes)
                .map_err(|error| {
                    let request_id = RequestId::Number(id as i32);
                    self.failure_reporter.report(
                        crate::ConnectionFailureCategory::Protocol,
                        Some(crate::ConnectionDirection::Inbound),
                        Some(R::METHOD),
                        Some(&request_id),
                    );
                    ClientError::Deserialize(error)
                }),
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
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::Serialize`],
    /// [`ClientError::ConnectionClosed`], [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`],
    /// or [`ClientError::IdExhausted`] if the request never reaches the wire;
    /// [`ClientError::Remote`] if the client answers with a JSON-RPC error;
    /// [`ClientError::Deserialize`] if the success result cannot be decoded;
    /// [`ClientError::Cancelled`] if the session closes before the client
    /// answers; [`ClientError::Timeout`] if the configured deadline expires.
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
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::Serialize`],
    /// [`ClientError::ConnectionClosed`], [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`],
    /// or [`ClientError::IdExhausted`] if the request never reaches the wire;
    /// [`ClientError::Remote`] if the client answers with a JSON-RPC error;
    /// [`ClientError::Deserialize`] if the success result cannot be decoded;
    /// [`ClientError::Cancelled`] if the session closes before the client
    /// answers; [`ClientError::Timeout`] if the configured deadline expires.
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
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::Serialize`],
    /// [`ClientError::ConnectionClosed`], [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`],
    /// or [`ClientError::IdExhausted`] if the request never reaches the wire;
    /// [`ClientError::Remote`] if the client answers with a JSON-RPC error;
    /// [`ClientError::Deserialize`] if the success result cannot be decoded;
    /// [`ClientError::Cancelled`] if the session closes before the client
    /// answers; [`ClientError::Timeout`] if the configured deadline expires.
    pub async fn apply_edit(
        &self,
        params: ApplyWorkspaceEditParams,
    ) -> Result<ApplyWorkspaceEditResponse, ClientError> {
        self.request::<ApplyWorkspaceEdit>(params).await
    }

    /// Ask the client for its configuration with `workspace/configuration`
    /// (LSP 3.17), awaiting one value per requested [`ConfigurationParams`]
    /// item.
    ///
    /// The items are sent exactly as provided: which sections to fetch and
    /// under which scope URIs all come from the caller. The client's
    /// `Vec<Value>` is returned verbatim to the caller — entries keep the
    /// client's order and length, with `null` wherever the client answered
    /// nothing. The query result is never written into the framework-owned
    /// [`Workspace`](crate::Workspace) configuration snapshot, which only
    /// tracks `workspace/didChangeConfiguration`.
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::Serialize`],
    /// [`ClientError::ConnectionClosed`], [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`],
    /// or [`ClientError::IdExhausted`] if the request never reaches the wire;
    /// [`ClientError::Remote`] if the client answers with a JSON-RPC error;
    /// [`ClientError::Deserialize`] if the success result cannot be decoded;
    /// [`ClientError::Cancelled`] if the session closes before the client
    /// answers; [`ClientError::Timeout`] if the configured deadline expires.
    pub async fn configuration(
        &self,
        params: ConfigurationParams,
    ) -> Result<Vec<serde_json::Value>, ClientError> {
        self.request::<WorkspaceConfiguration>(params).await
    }

    /// Ask the client for its current workspace folders with
    /// `workspace/workspaceFolders` (LSP 3.17), awaiting the client's
    /// `Option<Vec<WorkspaceFolder>>`.
    ///
    /// The client's folders are returned verbatim to the caller: `None` when
    /// the client answers `null` (a single untitled document), otherwise one
    /// folder per entry in the client's order. The query result is never
    /// written into the framework-owned [`Workspace`](crate::Workspace)
    /// folder list, which only tracks the initialization announcement and
    /// later `workspace/didChangeWorkspaceFolders` synchronization.
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::ConnectionClosed`],
    /// [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`], or [`ClientError::IdExhausted`] if the
    /// request never reaches the wire; [`ClientError::Remote`] if the client
    /// answers with a JSON-RPC error; [`ClientError::Deserialize`] if the
    /// success result cannot be decoded; [`ClientError::Cancelled`] if the
    /// session closes before the client answers; [`ClientError::Timeout`] if
    /// the configured deadline expires.
    pub async fn workspace_folders(&self) -> Result<Option<Vec<WorkspaceFolder>>, ClientError> {
        self.request::<WorkspaceFoldersRequest>(()).await
    }

    /// Tell the client about new capabilities with `client/registerCapability`
    /// (LSP 3.17), awaiting the client's acknowledgement.
    ///
    /// The [`RegistrationParams`] are sent exactly as provided: the
    /// registration ids, methods, and register options all come from the
    /// caller. The announcement changes nothing on the server side: the helper
    /// never adds, replaces, or removes a route in the connection's frozen
    /// Router, never recomputes the initialize capabilities, and the framework
    /// retains no second list of currently registered client capabilities.
    /// Any local route must already exist through static or
    /// initialize-conditional registration — dynamic registration only tells
    /// the client to start sending traffic for it. The client's `null`
    /// acknowledgement is returned as `()`.
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::Serialize`],
    /// [`ClientError::ConnectionClosed`], [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`],
    /// or [`ClientError::IdExhausted`] if the request never reaches the wire;
    /// [`ClientError::Remote`] if the client answers with a JSON-RPC error;
    /// [`ClientError::Deserialize`] if the success result cannot be decoded;
    /// [`ClientError::Cancelled`] if the session closes before the client
    /// answers; [`ClientError::Timeout`] if the configured deadline expires.
    pub async fn register_capability(&self, params: RegistrationParams) -> Result<(), ClientError> {
        self.request::<RegisterCapability>(params).await
    }

    /// Tell the client to drop capabilities with
    /// `client/unregisterCapability` (LSP 3.17), awaiting the client's
    /// acknowledgement.
    ///
    /// The [`UnregistrationParams`] are sent exactly as provided: which
    /// registration ids and methods to withdraw all come from the caller. The
    /// announcement changes nothing on the server side: the helper never adds,
    /// replaces, or removes a route in the connection's frozen Router, never
    /// recomputes the initialize capabilities, and the framework retains no
    /// second list of currently registered client capabilities. Any local
    /// route must already exist through static or initialize-conditional
    /// registration — dynamic unregistration only tells the client to stop
    /// sending traffic for it. The client's `null` acknowledgement is
    /// returned as `()`.
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::Serialize`],
    /// [`ClientError::ConnectionClosed`], [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`],
    /// or [`ClientError::IdExhausted`] if the request never reaches the wire;
    /// [`ClientError::Remote`] if the client answers with a JSON-RPC error;
    /// [`ClientError::Deserialize`] if the success result cannot be decoded;
    /// [`ClientError::Cancelled`] if the session closes before the client
    /// answers; [`ClientError::Timeout`] if the configured deadline expires.
    pub async fn unregister_capability(
        &self,
        params: UnregistrationParams,
    ) -> Result<(), ClientError> {
        self.request::<UnregisterCapability>(params).await
    }

    /// Ask the client to recompute its code lenses with
    /// `workspace/codeLens/refresh` (stable since LSP 3.16), awaiting the
    /// client's `null` acknowledgement as `()`.
    ///
    /// The request carries no parameters (`Params = ()`, sent as `null`); the
    /// helper owns no recomputation policy — which lenses the client recomputes
    /// is entirely the client's concern. The framework keeps no code-lens
    /// state, so nothing local changes when the client acknowledges.
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::ConnectionClosed`],
    /// [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`], or [`ClientError::IdExhausted`] if the
    /// request never reaches the wire; [`ClientError::Remote`] if the client
    /// answers with a JSON-RPC error; [`ClientError::Deserialize`] if the
    /// success result cannot be decoded; [`ClientError::Cancelled`] if the
    /// session closes before the client answers; [`ClientError::Timeout`] if
    /// the configured deadline expires.
    pub async fn code_lens_refresh(&self) -> Result<(), ClientError> {
        self.request::<CodeLensRefresh>(()).await
    }

    /// Ask the client to recompute its workspace diagnostics with
    /// `workspace/diagnostic/refresh` (stable since LSP 3.17), awaiting the
    /// client's `null` acknowledgement as `()`.
    ///
    /// The request carries no parameters (`Params = ()`, sent as `null`); the
    /// helper owns no recomputation policy — which documents the client re-pulls
    /// diagnostics for is entirely the client's concern. The framework keeps no
    /// pull-diagnostic state, so nothing local changes when the client
    /// acknowledges.
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::ConnectionClosed`],
    /// [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`], or [`ClientError::IdExhausted`] if the
    /// request never reaches the wire; [`ClientError::Remote`] if the client
    /// answers with a JSON-RPC error; [`ClientError::Deserialize`] if the
    /// success result cannot be decoded; [`ClientError::Cancelled`] if the
    /// session closes before the client answers; [`ClientError::Timeout`] if
    /// the configured deadline expires.
    pub async fn diagnostic_refresh(&self) -> Result<(), ClientError> {
        self.request::<WorkspaceDiagnosticRefresh>(()).await
    }

    /// Ask the client to recompute its inlay hints with
    /// `workspace/inlayHint/refresh` (stable since LSP 3.17), awaiting the
    /// client's `null` acknowledgement as `()`.
    ///
    /// The request carries no parameters (`Params = ()`, sent as `null`); the
    /// helper owns no recomputation policy — which hints the client recomputes
    /// is entirely the client's concern. The framework keeps no inlay-hint
    /// state, so nothing local changes when the client acknowledges.
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::ConnectionClosed`],
    /// [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`], or [`ClientError::IdExhausted`] if the
    /// request never reaches the wire; [`ClientError::Remote`] if the client
    /// answers with a JSON-RPC error; [`ClientError::Deserialize`] if the
    /// success result cannot be decoded; [`ClientError::Cancelled`] if the
    /// session closes before the client answers; [`ClientError::Timeout`] if
    /// the configured deadline expires.
    pub async fn inlay_hint_refresh(&self) -> Result<(), ClientError> {
        self.request::<InlayHintRefreshRequest>(()).await
    }

    /// Ask the client to recompute its inline values with
    /// `workspace/inlineValue/refresh` (stable since LSP 3.17), awaiting the
    /// client's `null` acknowledgement as `()`.
    ///
    /// The request carries no parameters (`Params = ()`, sent as `null`); the
    /// helper owns no recomputation policy — which values the client recomputes
    /// is entirely the client's concern. The framework keeps no inline-value
    /// state, so nothing local changes when the client acknowledges.
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::ConnectionClosed`],
    /// [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`], or [`ClientError::IdExhausted`] if the
    /// request never reaches the wire; [`ClientError::Remote`] if the client
    /// answers with a JSON-RPC error; [`ClientError::Deserialize`] if the
    /// success result cannot be decoded; [`ClientError::Cancelled`] if the
    /// session closes before the client answers; [`ClientError::Timeout`] if
    /// the configured deadline expires.
    pub async fn inline_value_refresh(&self) -> Result<(), ClientError> {
        self.request::<InlineValueRefreshRequest>(()).await
    }

    /// Ask the client to recompute its semantic tokens with
    /// `workspace/semanticTokens/refresh` (stable since LSP 3.16), awaiting
    /// the client's `null` acknowledgement as `()`.
    ///
    /// The request carries no parameters (`Params = ()`, sent as `null`); the
    /// helper owns no recomputation policy — which tokens the client recomputes
    /// is entirely the client's concern. The framework keeps no semantic-token
    /// state, so nothing local changes when the client acknowledges.
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::ConnectionClosed`],
    /// [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`], or [`ClientError::IdExhausted`] if the
    /// request never reaches the wire; [`ClientError::Remote`] if the client
    /// answers with a JSON-RPC error; [`ClientError::Deserialize`] if the
    /// success result cannot be decoded; [`ClientError::Cancelled`] if the
    /// session closes before the client answers; [`ClientError::Timeout`] if
    /// the configured deadline expires.
    pub async fn semantic_tokens_refresh(&self) -> Result<(), ClientError> {
        self.request::<SemanticTokensRefresh>(()).await
    }

    /// Ask the client to recompute its folding ranges with
    /// `workspace/foldingRange/refresh` (proposed LSP), awaiting the client's
    /// `null` acknowledgement as `()`.
    ///
    /// The request carries no parameters (`Params = ()`, sent as `null`); the
    /// helper owns no recomputation policy — which ranges the client
    /// recomputes is entirely the client's concern. The framework keeps no
    /// folding-range state, so nothing local changes when the client
    /// acknowledges.
    ///
    /// This helper exists only with the crate's `proposed` Cargo feature:
    /// `lsp-types` 0.97.x has no marker for this request, so the marker comes
    /// from [`crate::proposed`].
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::ConnectionClosed`],
    /// [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`], or [`ClientError::IdExhausted`] if the
    /// request never reaches the wire; [`ClientError::Remote`] if the client
    /// answers with a JSON-RPC error; [`ClientError::Deserialize`] if the
    /// success result cannot be decoded; [`ClientError::Cancelled`] if the
    /// session closes before the client answers; [`ClientError::Timeout`] if
    /// the configured deadline expires.
    #[cfg(feature = "proposed")]
    pub async fn refresh_folding_ranges(&self) -> Result<(), ClientError> {
        self.request::<crate::proposed::FoldingRangeRefresh>(())
            .await
    }

    /// Ask the client to refresh one document's cached content with
    /// `workspace/textDocumentContent/refresh` (proposed LSP), awaiting the
    /// client's `null` acknowledgement as `()`.
    ///
    /// The [`TextDocumentContentRefreshParams`](crate::proposed::TextDocumentContentRefreshParams)
    /// are sent exactly as provided: they name only the target document's URI
    /// and the helper owns no refresh policy — how the client re-pulls the
    /// content is entirely the client's concern. The framework keeps no
    /// text-document-content pull state, so nothing local changes when the
    /// client acknowledges.
    ///
    /// This helper exists only with the crate's `proposed` Cargo feature:
    /// `lsp-types` 0.97.x has no marker or params type for this request, so
    /// both come from [`crate::proposed`].
    ///
    /// # Errors
    ///
    /// Behaves exactly as [`ClientHandle::request`]: [`ClientError::ConnectionClosed`],
    /// [`ClientError::OutboundClosed`], [`ClientError::OutboundOverloaded`], or [`ClientError::IdExhausted`] if the
    /// request never reaches the wire; [`ClientError::Remote`] if the client
    /// answers with a JSON-RPC error; [`ClientError::Deserialize`] if the
    /// success result cannot be decoded; [`ClientError::Cancelled`] if the
    /// session closes before the client answers; [`ClientError::Timeout`] if
    /// the configured deadline expires.
    #[cfg(feature = "proposed")]
    pub async fn refresh_text_document_content(
        &self,
        params: crate::proposed::TextDocumentContentRefreshParams,
    ) -> Result<(), ClientError> {
        self.request::<crate::proposed::TextDocumentContentRefresh>(params)
            .await
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

    /// The writer's send-loop reports each finished message here, after its
    /// transport send succeeded or failed, so the queue depth counts only what
    /// is still waiting to be written.
    pub(crate) fn record_done(&self) {
        self.outgoing.record_done();
    }

    pub(crate) fn discard_outbound(&self) {
        self.outgoing.discard_all();
    }

    /// Engine-private accessor to the shared outbound registry.
    pub(crate) fn outbound_registry(&self) -> &OutboundRegistry {
        &self.outbound
    }

    /// Accessor to the connection's work-done progress token registry, shared
    /// by every clone of this handle.
    pub(crate) fn progress_registry(&self) -> &ProgressRegistry {
        &self.progress
    }

    /// The connection's shared trace level, handed to the
    /// [`Workspace`](crate::Workspace) when the engine establishes it, so
    /// `$/setTrace` writes and [`ClientHandle::log_trace`] reads observe one cell.
    pub(crate) fn shared_trace(&self) -> SharedTrace {
        self.trace.clone()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use lsp_types::NumberOrString;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::json;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    enum TestNotification {}

    impl Notification for TestNotification {
        type Params = serde_json::Value;
        const METHOD: &'static str = "test/notification";
    }

    enum TestRequest {}

    impl Request for TestRequest {
        type Params = serde_json::Value;
        type Result = String;
        const METHOD: &'static str = "test/request";
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

    fn make_client() -> (ClientHandle, UnboundedReceiver<RawMessage>) {
        let (outgoing, receiver) = OutboundQueue::new(crate::DEFAULT_OUTBOUND_WARNING_THRESHOLD);
        let client = ClientHandle::new(outgoing, OutboundRegistry::default(), None);
        (client, receiver)
    }

    #[test]
    fn serialization_failure_is_reported_without_enqueuing() {
        let (outgoing, mut receiver) =
            OutboundQueue::new(crate::DEFAULT_OUTBOUND_WARNING_THRESHOLD);
        let client = ClientHandle::new(outgoing, OutboundRegistry::default(), None);

        assert!(matches!(
            client.notify::<FailingNotification>(FailsToSerialize),
            Err(ClientError::Serialize(_))
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn closed_connection_is_reported_before_enqueue() {
        let (outgoing, mut receiver) =
            OutboundQueue::new(crate::DEFAULT_OUTBOUND_WARNING_THRESHOLD);
        let client = ClientHandle::new(outgoing, OutboundRegistry::default(), None);
        client.close_connection();

        assert!(matches!(
            client.notify::<TestNotification>(json!({ "value": 1 })),
            Err(ClientError::ConnectionClosed)
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn outbound_closure_rejects_every_new_notification() {
        let (outgoing, mut receiver) =
            OutboundQueue::new(crate::DEFAULT_OUTBOUND_WARNING_THRESHOLD);
        let client = ClientHandle::new(outgoing, OutboundRegistry::default(), None);
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
        assert!(matches!(
            rx1.try_recv().unwrap().unwrap(),
            PendingOutcome::Cancelled
        ));
        assert!(matches!(
            rx2.try_recv().unwrap().unwrap(),
            PendingOutcome::Cancelled
        ));
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
        let (outgoing, receiver) = OutboundQueue::new(crate::DEFAULT_OUTBOUND_WARNING_THRESHOLD);
        let client = ClientHandle::new(outgoing, OutboundRegistry::default(), None);
        drop(receiver);
        let client2 = client.clone();

        let result = client2.request::<PingRequest>(json!({})).await;
        assert!(matches!(result, Err(ClientError::OutboundClosed)));

        // Nothing was ever emitted, and the registry is empty.
        assert_eq!(client.outbound_registry().pending_len(), 0);
    }

    // --- OutboundQueue depth observability tests -----------------------------

    fn queue_test_message() -> RawMessage {
        RawMessage::Notification {
            method: Cow::Borrowed("test/message"),
            params: Bytes::from_static(b"{}"),
        }
    }

    /// Run `f` with an [`EventCapture`] subscriber, returning its result and
    /// every tracing event emitted on this thread.
    fn capture<T>(f: impl FnOnce() -> T) -> (T, crate::test_util::EventCapture) {
        let _capture = crate::test_util::tracing_capture_lock();
        let events = crate::test_util::EventCapture::new();
        let subscriber = tracing_subscriber::registry().with(events.clone());
        let result = tracing::subscriber::with_default(subscriber, f);
        (result, events)
    }

    fn warning_count(events: &crate::test_util::EventCapture) -> usize {
        events.count_at(tracing::Level::WARN, "reached the warning threshold")
    }

    #[test]
    fn the_first_threshold_crossing_records_the_depth_and_warns_once() {
        let (queue, _rx) = OutboundQueue::new(2);

        let ((), events) = capture(|| {
            queue.send(queue_test_message()).unwrap();
            queue.send(queue_test_message()).unwrap();
            queue.send(queue_test_message()).unwrap();
        });

        assert_eq!(queue.depth(), 3);
        assert!(
            !events.contains_at(tracing::Level::TRACE, "outbound.queue_depth=1"),
            "below the threshold a healthy queue stays silent, got {:?}",
            events.messages()
        );
        assert!(
            events.contains_at(tracing::Level::TRACE, "outbound.queue_depth=3"),
            "at and above the threshold every enqueue records the current depth, got {:?}",
            events.messages()
        );
        assert_eq!(
            events.count_at(tracing::Level::WARN, "outbound.queue_depth=2"),
            1,
            "the crossing warns once, recording the current depth, got {:?}",
            events.messages()
        );
    }

    #[test]
    fn sustained_depth_at_or_above_the_threshold_does_not_repeat_the_warning() {
        let (queue, _rx) = OutboundQueue::new(2);

        let ((), events) = capture(|| {
            for _ in 0..5 {
                queue.send(queue_test_message()).unwrap();
            }
        });

        assert_eq!(
            queue.depth(),
            5,
            "the queue stays unbounded above the threshold"
        );
        assert_eq!(
            warning_count(&events),
            1,
            "depth remaining at or above the threshold warns only once, got {:?}",
            events.messages()
        );
    }

    #[test]
    fn dropping_below_the_threshold_rearms_one_warning_for_the_next_crossing() {
        let (queue, _rx) = OutboundQueue::new(2);

        let ((), events) = capture(|| {
            queue.send(queue_test_message()).unwrap();
            queue.send(queue_test_message()).unwrap();
            queue.record_done();
            queue.record_done();
            assert_eq!(queue.depth(), 0, "the writer drained both messages");
            queue.send(queue_test_message()).unwrap();
            queue.send(queue_test_message()).unwrap();
            queue.send(queue_test_message()).unwrap();
        });

        assert_eq!(queue.depth(), 3);
        assert_eq!(
            events.count_at(tracing::Level::TRACE, "outbound.queue_depth=2"),
            2,
            "each crossing records the current depth at the threshold, got {:?}",
            events.messages()
        );
        assert!(
            !events.contains_at(tracing::Level::TRACE, "outbound.queue_depth=0"),
            "below the threshold the writer's sends stay silent, got {:?}",
            events.messages()
        );
        assert_eq!(
            warning_count(&events),
            2,
            "the recovery rearms exactly one warning for the second crossing, got {:?}",
            events.messages()
        );
    }

    #[tokio::test]
    async fn concurrent_enqueues_all_count_toward_the_one_shared_depth() {
        let (queue, mut rx) = OutboundQueue::new(usize::MAX);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let queue = queue.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..100 {
                    queue.send(queue_test_message()).unwrap();
                }
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        assert_eq!(queue.depth(), 800, "no concurrent enqueue is lost");
        for _ in 0..800 {
            rx.try_recv().expect("no enqueued message is dropped");
        }
        assert!(
            rx.try_recv().is_err(),
            "exactly the enqueued messages arrive"
        );
    }

    #[test]
    fn a_failed_enqueue_is_not_counted() {
        let (queue, rx) = OutboundQueue::new(1);
        drop(rx);

        assert!(queue.send(queue_test_message()).is_err());
        assert_eq!(
            queue.depth(),
            0,
            "a message that never entered the queue is not counted"
        );
    }

    #[test]
    fn ordinary_sends_fail_explicitly_when_the_message_budget_is_full() {
        let (queue, _rx) = OutboundQueue::bounded(1, usize::MAX);
        let client = ClientHandle::new(queue.clone(), OutboundRegistry::default(), None);

        client
            .notify::<TestNotification>(json!({ "value": 1 }))
            .unwrap();
        let error = client
            .notify::<TestNotification>(json!({ "value": 2 }))
            .unwrap_err();

        assert!(matches!(error, ClientError::OutboundOverloaded));
        assert_eq!(queue.depth(), 1, "the rejected message consumes no slot");
    }

    #[test]
    fn every_queued_message_accounts_for_its_encoded_bytes() {
        let message = queue_test_message();
        let encoded_bytes = crate::transport::envelope::serialize(&message)
            .expect("the test message encodes")
            .len();
        let (queue, _rx) = OutboundQueue::bounded(2, encoded_bytes);

        queue.send(message).unwrap();
        assert_eq!(queue.encoded_bytes(), encoded_bytes);
        assert!(matches!(
            queue.send(queue_test_message()),
            Err(OutboundSendError::Overloaded)
        ));
        assert_eq!(queue.depth(), 1, "the rejected message consumes no slot");
        assert_eq!(
            queue.encoded_bytes(),
            encoded_bytes,
            "the rejected message consumes no bytes"
        );
    }

    #[test]
    fn a_required_send_that_cannot_fit_requests_the_failure_close_path() {
        let (queue, _rx) = OutboundQueue::bounded(1, usize::MAX);
        queue.send(queue_test_message()).unwrap();

        assert!(matches!(
            queue.send_required(queue_test_message()),
            Err(OutboundSendError::Overloaded)
        ));
        assert!(
            queue.failure().is_cancelled(),
            "a response or cancellation that cannot be queued must close the connection"
        );
        assert_eq!(queue.depth(), 1, "the failed required send is not retained");
    }

    #[tokio::test]
    async fn an_overloaded_request_fails_explicitly_without_leaking_its_registry_entry() {
        let (queue, _rx) = OutboundQueue::bounded(1, usize::MAX);
        let client = ClientHandle::new(queue, OutboundRegistry::default(), None);
        client
            .notify::<TestNotification>(json!({ "value": 1 }))
            .unwrap();

        let result = client.request::<TestRequest>(json!({})).await;

        assert!(matches!(result, Err(ClientError::OutboundOverloaded)));
        assert_eq!(client.outbound_registry().pending_len(), 0);
    }

    #[tokio::test]
    async fn cancellation_that_cannot_fit_requests_the_failure_close_path() {
        let (queue, _rx) = OutboundQueue::bounded(1, usize::MAX);
        let client = ClientHandle::new(queue.clone(), OutboundRegistry::default(), None);
        {
            let pending = client.request::<TestRequest>(json!({}));
            futures_util::pin_mut!(pending);
            assert!(futures_util::poll!(pending.as_mut()).is_pending());
        }

        assert_eq!(client.outbound_registry().pending_len(), 0);
        assert!(
            queue.failure().is_cancelled(),
            "a required $/cancelRequest that cannot fit must close the connection"
        );
    }
}
