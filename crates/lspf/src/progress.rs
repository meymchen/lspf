//! Connection-scoped work-done progress lifecycle (LSP 3.17
//! `window/workDoneProgress/*` and `$/progress`).
//!
//! [`ClientHandle::begin_progress`] performs the whole begin sequence as one
//! failure-safe operation: it allocates a connection-local numeric token,
//! completes `window/workDoneProgress/create`, registers the token only after
//! the remote success, and enqueues exactly one work-done begin notification.
//! The returned [`ProgressHandle`] reports and ends the progress; dropping an
//! active handle reclaims its token without performing any I/O.
//!
//! The [`ProgressRegistry`] is the connection-local registry of active
//! progress tokens. It is shared by every [`ClientHandle`] clone of the connection
//! and is independent of the outbound request-ID allocator: progress tokens
//! and request IDs are separate monotonic sequences. The protocol engine's
//! `window/workDoneProgress/cancel` built-in resolves a client cancellation
//! against it, and session close clears it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gen_lsp_types::{
    ProgressNotification as Progress, ProgressParams, ProgressToken, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressCreateRequest as WorkDoneProgressCreate,
    WorkDoneProgressEnd, WorkDoneProgressReport,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::client::ClientHandle;
use crate::error::{ClientError, ProgressError};

/// The largest token value handed out: progress tokens travel as JSON
/// integers, so the connection-local sequence stays within positive `i32`.
const MAX_PROGRESS_TOKEN: u32 = i32::MAX as u32;

fn progress_value(value: impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("generated progress values serialize")
}

/// One active progress entry in the connection's registry.
pub(crate) struct ActiveProgress {
    /// Whether the begin announcement told the client the operation is
    /// cancellable.
    pub(crate) cancellable: bool,
    /// The handle's cancellation token, triggered by the
    /// `window/workDoneProgress/cancel` built-in. User code may also cancel
    /// it directly.
    pub(crate) cancellation: CancellationToken,
}

/// The outcome of applying one `window/workDoneProgress/cancel` notification
/// to the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProgressCancel {
    /// The token was active and cancellable; its cancellation token fired.
    Cancelled,
    /// The token is active but its begin announcement was not cancellable.
    NotCancellable,
    /// The token is not active on the connection — unknown or already ended.
    NotActive,
}

/// The connection-local registry of active work-done progress tokens.
///
/// Tokens are allocated from a monotonically increasing numeric sequence
/// starting at 1 that never reuses a value and skips any token already
/// active on the connection — server-originated ones handed out by
/// [`ClientHandle::begin_progress`] and client-originated ones alike. Allocation is
/// independent of the outbound request-ID allocator.
///
/// Allocation and registration are separate steps: [`ClientHandle::begin_progress`]
/// registers its token only after `window/workDoneProgress/create` succeeds,
/// so a failed create leaves no registered token behind.
#[derive(Clone, Default)]
pub(crate) struct ProgressRegistry {
    inner: Arc<Mutex<ProgressInner>>,
}

struct ProgressInner {
    /// The next candidate token number; never decreases.
    next_token: u32,
    /// Active tokens, server- or client-originated.
    active: HashMap<ProgressToken, ActiveProgress>,
}

impl Default for ProgressInner {
    fn default() -> Self {
        Self {
            next_token: 1,
            active: HashMap::new(),
        }
    }
}

impl ProgressRegistry {
    /// Pick the next free numeric token without registering it.
    ///
    /// The sequence starts at 1 and advances monotonically; numbers already
    /// active on the connection are skipped. Returns `None` once the positive
    /// `i32` token space is exhausted.
    pub(crate) fn allocate(&self) -> Option<ProgressToken> {
        let mut inner = self.inner.lock().unwrap();
        let mut candidate = inner.next_token;
        loop {
            if candidate > MAX_PROGRESS_TOKEN {
                return None;
            }
            let token = ProgressToken::Int(candidate as i32);
            if !inner.active.contains_key(&token) {
                inner.next_token = candidate + 1;
                return Some(token);
            }
            candidate += 1;
        }
    }

    /// Register an allocated (or client-originated) token as active.
    ///
    /// Returns `false` without mutating anything when the token is already
    /// active. Tokens handed out by [`ProgressRegistry::allocate`] can only
    /// collide with a client-originated registration that raced in between
    /// allocation and registration.
    pub(crate) fn register(
        &self,
        token: ProgressToken,
        cancellable: bool,
        cancellation: CancellationToken,
    ) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.active.contains_key(&token) {
            return false;
        }
        inner.active.insert(
            token,
            ActiveProgress {
                cancellable,
                cancellation,
            },
        );
        true
    }

    /// Remove an active token, returning its entry if it was registered.
    pub(crate) fn remove(&self, token: &ProgressToken) -> Option<ActiveProgress> {
        self.inner.lock().unwrap().active.remove(token)
    }

    /// Apply one client cancellation to `token` (the
    /// `window/workDoneProgress/cancel` built-in).
    ///
    /// A matching active and cancellable entry fires its cancellation token;
    /// the entry stays registered, because cancellation never ends the
    /// progress by itself — the application decides the final message and
    /// calls `end`. A non-cancellable or inactive (unknown or already ended)
    /// token leaves the registry untouched.
    pub(crate) fn cancel(&self, token: &ProgressToken) -> ProgressCancel {
        let inner = self.inner.lock().unwrap();
        match inner.active.get(token) {
            Some(entry) if entry.cancellable => {
                entry.cancellation.cancel();
                ProgressCancel::Cancelled
            }
            Some(_) => ProgressCancel::NotCancellable,
            None => ProgressCancel::NotActive,
        }
    }

    /// Remove every active token.
    ///
    /// Session close calls this so a closed connection holds no stale
    /// entries; handles that outlive the connection then observe
    /// [`ProgressError::UnknownToken`]. The registry is connection-owned, so
    /// clearing it cannot affect another connection.
    pub(crate) fn clear(&self) {
        self.inner.lock().unwrap().active.clear();
    }

    /// Whether the token is currently active on this connection.
    pub(crate) fn is_active(&self, token: &ProgressToken) -> bool {
        self.inner.lock().unwrap().active.contains_key(token)
    }

    /// Number of active tokens (test-only, for leak assertions).
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn active_len(&self) -> usize {
        self.inner.lock().unwrap().active.len()
    }

    /// Override the next candidate token (test-only, for exhaustion coverage).
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn set_next_token(&self, token: u32) {
        self.inner.lock().unwrap().next_token = token;
    }
}

/// Options for [`ClientHandle::begin_progress`], mapped verbatim onto the work-done
/// begin notification.
///
/// `cancellable` defaults to `false`; `message` and `percentage` default to
/// unset. A `percentage` outside the inclusive range 0 through 100 is
/// rejected with [`ClientError::InvalidHelperParams`] before any request or
/// notification is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressOptions {
    /// The mandatory begin title (for example `"Indexing"`).
    pub title: String,
    /// Whether the client may offer the user a cancel button.
    pub cancellable: bool,
    /// An optional complementary begin message.
    pub message: Option<String>,
    /// An optional begin percentage in the inclusive range 0 through 100.
    pub percentage: Option<u32>,
}

impl ProgressOptions {
    /// Options carrying only the mandatory title; `cancellable` is `false`
    /// and both `message` and `percentage` are unset.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            cancellable: false,
            message: None,
            percentage: None,
        }
    }

    /// Set whether the operation is cancellable.
    #[must_use]
    pub fn cancellable(mut self, cancellable: bool) -> Self {
        self.cancellable = cancellable;
        self
    }

    /// Set the begin message.
    #[must_use]
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Set the begin percentage; must be within 0 through 100 inclusive.
    #[must_use]
    pub fn percentage(mut self, percentage: u32) -> Self {
        self.percentage = Some(percentage);
        self
    }
}

/// The shared state behind one [`ProgressHandle`].
///
/// Cloning is crate-internal: tests and the protocol engine's
/// `window/workDoneProgress/cancel` built-in operate on the shared state
/// while the public handle owns the user-facing lifecycle. `end` on the
/// shared state is idempotent in the failure sense — a second call fails with
/// [`ProgressError::AlreadyEnded`] instead of sending a second end.
#[derive(Clone)]
pub(crate) struct SharedProgress {
    inner: Arc<SharedInner>,
}

struct SharedInner {
    client: ClientHandle,
    registry: ProgressRegistry,
    token: ProgressToken,
    cancellable: bool,
    cancellation: CancellationToken,
    ended: AtomicBool,
}

impl SharedProgress {
    fn new(
        client: ClientHandle,
        registry: ProgressRegistry,
        token: ProgressToken,
        cancellable: bool,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            inner: Arc::new(SharedInner {
                client,
                registry,
                token,
                cancellable,
                cancellation,
                ended: AtomicBool::new(false),
            }),
        }
    }

    fn is_ended(&self) -> bool {
        self.inner.ended.load(Ordering::SeqCst)
    }

    /// Enqueue one work-done report notification for the token.
    ///
    /// An ended handle fails with [`ProgressError::AlreadyEnded`], a cancelled
    /// one with [`ProgressError::Cancelled`] and a token no longer active on
    /// the connection with [`ProgressError::UnknownToken`] — all without
    /// sending anything. A percentage outside 0 through 100 fails with
    /// [`ProgressError::InvalidPercentage`]; percentages are otherwise sent as
    /// given, with no monotonicity enforcement.
    fn report(&self, message: Option<String>, percentage: Option<u32>) -> Result<(), ClientError> {
        if self.is_ended() {
            return Err(ProgressError::AlreadyEnded.into());
        }
        if self.inner.cancellation.is_cancelled() {
            return Err(ProgressError::Cancelled.into());
        }
        if !self.inner.registry.is_active(&self.inner.token) {
            return Err(ProgressError::UnknownToken.into());
        }
        if let Some(percentage) = percentage
            && percentage > 100
        {
            return Err(ProgressError::InvalidPercentage(percentage).into());
        }
        self.inner.client.notify_logged::<Progress>(ProgressParams {
            token: self.inner.token.clone(),
            value: progress_value(WorkDoneProgressReport {
                cancellable: Some(self.inner.cancellable),
                message,
                percentage,
            }),
        })
    }

    /// Enqueue one work-done end notification for the token and remove the
    /// token from the connection's registry.
    ///
    /// Only the first call sends: a repeated call fails with
    /// [`ProgressError::AlreadyEnded`]. The token is removed after the enqueue
    /// whether it succeeded or failed. A cancelled handle still ends — the
    /// application decides the final message.
    fn end(&self, message: Option<String>) -> Result<(), ClientError> {
        if self.inner.ended.swap(true, Ordering::SeqCst) {
            return Err(ProgressError::AlreadyEnded.into());
        }
        let result = self.inner.client.notify_logged::<Progress>(ProgressParams {
            token: self.inner.token.clone(),
            value: progress_value(WorkDoneProgressEnd { message }),
        });
        self.inner.registry.remove(&self.inner.token);
        result
    }
}

/// The connection-scoped handle for one work-done progress operation, created
/// with a server-allocated token by [`ClientHandle::begin_progress`] or with
/// the inbound request's token by
/// [`ServerContext::begin_progress`](crate::ServerContext::begin_progress).
///
/// The handle reports through [`ProgressHandle::report`] and finishes through
/// [`ProgressHandle::end`], which consumes it. Dropping an active handle
/// removes its token from the connection's registry and logs a warning, but
/// performs no I/O: no implicit end notification is sent.
pub struct ProgressHandle {
    shared: SharedProgress,
}

impl std::fmt::Debug for ProgressHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProgressHandle")
            .field("token", &self.shared.inner.token)
            .finish_non_exhaustive()
    }
}

impl ProgressHandle {
    /// The progress token allocated for this operation.
    pub fn token(&self) -> ProgressToken {
        self.shared.inner.token.clone()
    }

    /// The handle's cancellation token.
    ///
    /// User code may cancel it directly; the protocol engine's
    /// `window/workDoneProgress/cancel` built-in cancels it when the user
    /// cancels a cancellable progress in the client UI. Cancellation
    /// never sends anything by itself: reports on a cancelled handle fail
    /// with [`ProgressError::Cancelled`] and the application still decides
    /// the final message and calls [`ProgressHandle::end`].
    pub fn cancellation_token(&self) -> CancellationToken {
        self.shared.inner.cancellation.clone()
    }

    /// Enqueue one work-done report notification with the exact
    /// `WorkDoneProgressReport` shape: the handle's `cancellable` flag plus
    /// the given `message` and `percentage`.
    ///
    /// # Errors
    ///
    /// [`ClientError::Progress`] wrapping [`ProgressError::AlreadyEnded`],
    /// [`ProgressError::Cancelled`], or [`ProgressError::UnknownToken`] for a
    /// handle whose lifecycle already concluded, and
    /// [`ProgressError::InvalidPercentage`] for a percentage outside 0
    /// through 100 — none of these send anything. Enqueue failures surface as
    /// the other [`ClientError`] variants.
    pub fn report(
        &self,
        message: Option<String>,
        percentage: Option<u32>,
    ) -> Result<(), ClientError> {
        self.shared.report(message, percentage)
    }

    /// Enqueue one work-done end notification and remove the token from the
    /// connection's registry, consuming the handle.
    ///
    /// The token is removed after the enqueue whether it succeeded or failed,
    /// so the handle never leaks its registry entry. A cancelled handle still
    /// ends: cancellation never sends an implicit end by itself.
    ///
    /// # Errors
    ///
    /// Enqueue failures surface as [`ClientError`]; the token is removed
    /// regardless.
    pub fn end(self, message: Option<String>) -> Result<(), ClientError> {
        self.shared.end(message)
    }
}

impl Drop for ProgressHandle {
    fn drop(&mut self) {
        // No I/O here: an active handle that was never ended loses its token
        // registration and logs a warning, but no implicit end notification is
        // sent — the connection may already be closing.
        if !self.shared.is_ended() {
            self.shared.inner.registry.remove(&self.shared.inner.token);
            warn!(
                token = ?self.shared.inner.token,
                "progress handle dropped without end; token removed, no end notification sent"
            );
        }
    }
}

impl ClientHandle {
    pub(crate) fn begin_progress_with_token(
        &self,
        token: ProgressToken,
        options: ProgressOptions,
    ) -> Result<ProgressHandle, ClientError> {
        if let Some(percentage) = options.percentage
            && percentage > 100
        {
            return Err(ClientError::InvalidHelperParams(format!(
                "work-done begin percentage {percentage} is outside the range 0..=100"
            )));
        }

        let registry = self.progress_registry().clone();
        let cancellation = CancellationToken::new();
        if !registry.register(token.clone(), options.cancellable, cancellation.clone()) {
            return Err(ProgressError::UnknownToken.into());
        }

        let begin = ProgressParams {
            token: token.clone(),
            value: progress_value(WorkDoneProgressBegin {
                title: options.title,
                cancellable: Some(options.cancellable),
                message: options.message,
                percentage: options.percentage,
            }),
        };
        if let Err(error) = self.notify_logged::<Progress>(begin) {
            registry.remove(&token);
            return Err(error);
        }

        Ok(ProgressHandle {
            shared: SharedProgress::new(
                self.clone(),
                registry,
                token,
                options.cancellable,
                cancellation,
            ),
        })
    }

    /// Begin one connection-scoped work-done progress operation (LSP 3.17).
    ///
    /// The sequence is one failure-safe lifecycle: allocate a
    /// connection-local numeric token (monotonic from 1, skipping tokens
    /// already active on the connection, independent of outbound request
    /// IDs), complete `window/workDoneProgress/create`, register the token
    /// only after the remote success, then enqueue exactly one `$/progress`
    /// begin notification carrying the [`ProgressOptions`] verbatim.
    ///
    /// A create failure sends no begin notification and leaves no registered
    /// token; a begin enqueue failure removes the token again, so a failed
    /// begin never leaks its registry entry.
    ///
    /// # Errors
    ///
    /// - [`ClientError::InvalidHelperParams`] for a begin percentage outside
    ///   0 through 100, before any request or notification is sent;
    /// - the [`ClientHandle::request`] errors for the create step, including
    ///   [`ClientError::Remote`] when the client refuses the creation;
    /// - the [`ClientHandle::notify`] errors when the begin notification cannot be
    ///   enqueued.
    pub async fn begin_progress(
        &self,
        options: ProgressOptions,
    ) -> Result<ProgressHandle, ClientError> {
        if let Some(percentage) = options.percentage
            && percentage > 100
        {
            return Err(ClientError::InvalidHelperParams(format!(
                "work-done begin percentage {percentage} is outside the range 0..=100"
            )));
        }

        let registry = self.progress_registry().clone();
        let token = registry.allocate().ok_or(ClientError::IdExhausted)?;

        // The token is registered only after the remote success, so a failed
        // create sends no begin and leaves no registered token behind.
        self.request::<WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
            token: token.clone(),
        })
        .await?;

        self.begin_progress_with_token(token, options)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use bytes::Bytes;
    use futures_channel::mpsc;
    use serde_json::{Value, json};
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::LspError;
    use crate::client::{OutboundQueue, OutboundRegistry};
    use crate::raw::{RawMessage, RequestId};

    fn make_client() -> (
        ClientHandle,
        OutboundRegistry,
        mpsc::UnboundedReceiver<RawMessage>,
    ) {
        let (outgoing, receiver) =
            OutboundQueue::new(crate::ResourcePolicy::default().max_outbound_messages);
        let outbound = OutboundRegistry::default();
        let client = ClientHandle::new(outgoing, outbound.clone(), None);
        (client, outbound, receiver)
    }

    /// Read the next outbound request and complete it with a `null` success.
    async fn answer_create_ok(
        outbound: &OutboundRegistry,
        receiver: &mut mpsc::UnboundedReceiver<RawMessage>,
    ) -> Value {
        let message = receiver.recv().await.expect("create request");
        match message {
            RawMessage::Request { id, method, params } => {
                assert_eq!(method, "window/workDoneProgress/create");
                let RequestId::Number(id) = id else {
                    panic!("numeric request id");
                };
                let params: Value = serde_json::from_slice(&params).unwrap();
                assert!(outbound.complete(id as u32, Ok(Bytes::from_static(b"null"))));
                params
            }
            other => panic!("expected create request, got {other:?}"),
        }
    }

    async fn next_notification(
        receiver: &mut mpsc::UnboundedReceiver<RawMessage>,
    ) -> (String, Value) {
        match receiver.recv().await.expect("notification") {
            RawMessage::Notification { method, params } => (
                method.into_owned(),
                serde_json::from_slice(&params).unwrap(),
            ),
            other => panic!("expected notification, got {other:?}"),
        }
    }

    #[test]
    fn options_default_to_not_cancellable_without_message_or_percentage() {
        let options = ProgressOptions::new("Indexing");
        assert_eq!(options.title, "Indexing");
        assert!(!options.cancellable);
        assert_eq!(options.message, None);
        assert_eq!(options.percentage, None);

        let options = ProgressOptions::new("Indexing")
            .cancellable(true)
            .message("starting")
            .percentage(10);
        assert!(options.cancellable);
        assert_eq!(options.message.as_deref(), Some("starting"));
        assert_eq!(options.percentage, Some(10));
    }

    #[test]
    fn tokens_start_at_one_and_increase_monotonically() {
        let registry = ProgressRegistry::default();
        assert_eq!(registry.allocate(), Some(ProgressToken::Int(1)));
        assert_eq!(registry.allocate(), Some(ProgressToken::Int(2)));
        assert_eq!(registry.allocate(), Some(ProgressToken::Int(3)));
    }

    #[test]
    fn allocation_skips_active_server_and_client_originated_tokens() {
        let registry = ProgressRegistry::default();
        // Simulate a client-originated token colliding with the next number,
        // and a server-originated one right after it.
        assert!(registry.register(ProgressToken::Int(1), false, CancellationToken::new()));
        assert!(registry.register(ProgressToken::Int(2), false, CancellationToken::new()));
        // String tokens never collide with the numeric sequence.
        assert!(registry.register(
            ProgressToken::String("client-token".into()),
            false,
            CancellationToken::new()
        ));
        assert_eq!(registry.allocate(), Some(ProgressToken::Int(3)));

        // Removing an active token does not make its number reusable: the
        // allocator only moves forward.
        registry.remove(&ProgressToken::Int(1));
        assert_eq!(registry.allocate(), Some(ProgressToken::Int(4)));

        // Re-registering an active token fails without mutation.
        assert!(!registry.register(ProgressToken::Int(2), true, CancellationToken::new()));
    }

    #[test]
    fn allocation_exhausts_at_the_positive_i32_boundary() {
        let registry = ProgressRegistry::default();
        registry.set_next_token(MAX_PROGRESS_TOKEN);
        assert_eq!(registry.allocate(), Some(ProgressToken::Int(i32::MAX)));
        assert_eq!(registry.allocate(), None);
    }

    #[test]
    fn cancel_fires_only_a_matching_active_cancellable_token() {
        let registry = ProgressRegistry::default();
        let cancellable = CancellationToken::new();
        let plain = CancellationToken::new();
        assert!(registry.register(ProgressToken::Int(1), true, cancellable.clone()));
        assert!(registry.register(ProgressToken::Int(2), false, plain.clone()));

        // A non-cancellable token is left alone.
        assert_eq!(
            registry.cancel(&ProgressToken::Int(2)),
            ProgressCancel::NotCancellable
        );
        assert!(!plain.is_cancelled());

        // An inactive token — unknown or already ended — changes nothing.
        assert_eq!(
            registry.cancel(&ProgressToken::Int(3)),
            ProgressCancel::NotActive
        );
        assert_eq!(
            registry.cancel(&ProgressToken::String("ended".into())),
            ProgressCancel::NotActive
        );

        // The matching cancellable token fires and stays registered: ending
        // the progress remains the application's decision.
        assert_eq!(
            registry.cancel(&ProgressToken::Int(1)),
            ProgressCancel::Cancelled
        );
        assert!(cancellable.is_cancelled());
        assert!(registry.is_active(&ProgressToken::Int(1)));

        // A repeated cancel fires again and reports the same outcome.
        assert_eq!(
            registry.cancel(&ProgressToken::Int(1)),
            ProgressCancel::Cancelled
        );
    }

    #[test]
    fn clear_removes_every_active_token() {
        let registry = ProgressRegistry::default();
        let token = registry.allocate().expect("the token space is fresh");
        assert!(registry.register(token.clone(), true, CancellationToken::new()));
        assert!(registry.register(
            ProgressToken::String("client-token".into()),
            false,
            CancellationToken::new()
        ));

        registry.clear();

        assert!(!registry.is_active(&token));
        assert!(!registry.is_active(&ProgressToken::String("client-token".into())));
        assert_eq!(registry.active_len(), 0);
        // The token sequence is unaffected: allocation keeps moving forward.
        assert_eq!(registry.allocate(), Some(ProgressToken::Int(2)));
    }

    #[tokio::test]
    async fn begin_progress_happy_path_sends_create_then_one_begin() {
        let (client, outbound, mut receiver) = make_client();
        let registry = client.progress_registry().clone();

        let begin = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .begin_progress(
                        ProgressOptions::new("Indexing")
                            .cancellable(true)
                            .message("starting")
                            .percentage(0),
                    )
                    .await
            }
        });

        // The create request carries the first connection-local token.
        let params = answer_create_ok(&outbound, &mut receiver).await;
        assert_eq!(params, json!({ "token": 1 }));

        let handle = begin.await.unwrap().expect("begin succeeds");
        assert_eq!(handle.token(), ProgressToken::Int(1));
        assert!(!handle.cancellation_token().is_cancelled());

        // Exactly one begin notification with the verbatim options.
        let (method, params) = next_notification(&mut receiver).await;
        assert_eq!(
            method,
            <Progress as crate::types::notification::Notification>::METHOD
        );
        assert_eq!(
            params,
            json!({
                "token": 1,
                "value": {
                    "kind": "begin",
                    "title": "Indexing",
                    "cancellable": true,
                    "message": "starting",
                    "percentage": 0
                }
            })
        );
        assert!(registry.is_active(&ProgressToken::Int(1)));
        assert!(receiver.try_recv().is_err(), "nothing else is sent");

        handle.end(Some("done".into())).unwrap();
    }

    #[tokio::test]
    async fn invalid_begin_percentage_is_rejected_before_any_io() {
        let (client, _outbound, mut receiver) = make_client();
        let registry = client.progress_registry().clone();

        let error = client
            .begin_progress(ProgressOptions::new("Indexing").percentage(101))
            .await
            .unwrap_err();
        assert!(matches!(error, ClientError::InvalidHelperParams(_)));
        assert!(
            receiver.try_recv().is_err(),
            "no request or notification sent"
        );
        assert_eq!(registry.active_len(), 0, "no token registered");
    }

    #[tokio::test]
    async fn create_remote_failure_sends_no_begin_and_registers_no_token() {
        let (client, outbound, mut receiver) = make_client();
        let registry = client.progress_registry().clone();

        let begin = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .begin_progress(ProgressOptions::new("Indexing"))
                    .await
            }
        });

        // Answer the create request with a JSON-RPC error.
        match receiver.recv().await.expect("create request") {
            RawMessage::Request { id, .. } => {
                let RequestId::Number(id) = id else {
                    panic!("numeric id")
                };
                let failure = LspError::RequestFailed("Request failed".into());
                assert!(outbound.complete(
                    id as u32,
                    Err(crate::raw::JsonRpcError {
                        code: failure.code(),
                        message: failure.message(),
                        data: None,
                    })
                ));
            }
            other => panic!("expected create request, got {other:?}"),
        }

        let error = begin.await.unwrap().unwrap_err();
        assert!(matches!(error, ClientError::Remote(_)));
        assert!(receiver.try_recv().is_err(), "no begin notification sent");
        assert_eq!(registry.active_len(), 0, "no token left registered");
    }

    #[tokio::test]
    async fn begin_enqueue_failure_removes_the_token() {
        let (client, outbound, mut receiver) = make_client();
        let registry = client.progress_registry().clone();

        let begin = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .begin_progress(ProgressOptions::new("Indexing"))
                    .await
            }
        });
        answer_create_ok(&outbound, &mut receiver).await;

        // Close the outbound queue before the begin notification is enqueued.
        drop(receiver);
        let error = begin.await.unwrap().unwrap_err();
        assert!(matches!(error, ClientError::OutboundClosed));
        assert_eq!(registry.active_len(), 0, "begin failure removed the token");
    }

    #[tokio::test]
    async fn report_sends_the_exact_work_done_report_shape() {
        let (client, outbound, mut receiver) = make_client();
        let begin = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .begin_progress(ProgressOptions::new("Indexing").cancellable(true))
                    .await
            }
        });
        answer_create_ok(&outbound, &mut receiver).await;
        let handle = begin.await.unwrap().unwrap();
        next_notification(&mut receiver).await; // begin

        // Boundary percentages are accepted; monotonicity is not enforced.
        handle.report(Some("half".into()), Some(50)).unwrap();
        handle.report(None, Some(100)).unwrap();
        handle.report(Some("restarted".into()), Some(0)).unwrap();

        let (method, params) = next_notification(&mut receiver).await;
        assert_eq!(
            method,
            <Progress as crate::types::notification::Notification>::METHOD
        );
        assert_eq!(
            params,
            json!({
                "token": 1,
                "value": {
                    "kind": "report",
                    "cancellable": true,
                    "message": "half",
                    "percentage": 50
                }
            })
        );
        let (_, params) = next_notification(&mut receiver).await;
        assert_eq!(
            params,
            json!({ "token": 1, "value": { "kind": "report", "cancellable": true, "percentage": 100 } })
        );
        let (_, params) = next_notification(&mut receiver).await;
        assert_eq!(
            params,
            json!({
                "token": 1,
                "value": { "kind": "report", "cancellable": true, "message": "restarted", "percentage": 0 }
            })
        );

        handle.end(None).unwrap();
    }

    #[tokio::test]
    async fn invalid_report_percentage_sends_nothing() {
        let (client, outbound, mut receiver) = make_client();
        let begin = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .begin_progress(ProgressOptions::new("Indexing"))
                    .await
            }
        });
        answer_create_ok(&outbound, &mut receiver).await;
        let handle = begin.await.unwrap().unwrap();
        next_notification(&mut receiver).await; // begin

        let error = handle.report(None, Some(101)).unwrap_err();
        assert!(matches!(
            error,
            ClientError::Progress(ProgressError::InvalidPercentage(101))
        ));
        assert!(receiver.try_recv().is_err(), "invalid report sent nothing");

        handle.end(None).unwrap();
    }

    #[tokio::test]
    async fn cancelled_handle_reports_nothing_but_can_still_end() {
        let (client, outbound, mut receiver) = make_client();
        let begin = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .begin_progress(ProgressOptions::new("Indexing").cancellable(true))
                    .await
            }
        });
        answer_create_ok(&outbound, &mut receiver).await;
        let handle = begin.await.unwrap().unwrap();
        next_notification(&mut receiver).await; // begin

        handle.cancellation_token().cancel();
        let error = handle.report(None, Some(10)).unwrap_err();
        assert!(matches!(
            error,
            ClientError::Progress(ProgressError::Cancelled)
        ));
        assert!(
            receiver.try_recv().is_err(),
            "cancelled report sent nothing"
        );

        // Cancellation never sends an implicit end: the application decides
        // the final message and ends the progress itself.
        handle.end(Some("stopped".into())).unwrap();
        let (method, params) = next_notification(&mut receiver).await;
        assert_eq!(
            method,
            <Progress as crate::types::notification::Notification>::METHOD
        );
        assert_eq!(
            params,
            json!({ "token": 1, "value": { "kind": "end", "message": "stopped" } })
        );
    }

    #[tokio::test]
    async fn end_consumes_the_handle_and_removes_the_token() {
        let (client, outbound, mut receiver) = make_client();
        let registry = client.progress_registry().clone();
        let begin = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .begin_progress(ProgressOptions::new("Indexing"))
                    .await
            }
        });
        answer_create_ok(&outbound, &mut receiver).await;
        let handle = begin.await.unwrap().unwrap();
        next_notification(&mut receiver).await; // begin
        assert!(registry.is_active(&ProgressToken::Int(1)));

        handle.end(Some("done".into())).unwrap();
        let (method, params) = next_notification(&mut receiver).await;
        assert_eq!(
            method,
            <Progress as crate::types::notification::Notification>::METHOD
        );
        assert_eq!(
            params,
            json!({ "token": 1, "value": { "kind": "end", "message": "done" } })
        );
        assert!(!registry.is_active(&ProgressToken::Int(1)));
        assert_eq!(registry.active_len(), 0);
    }

    #[tokio::test]
    async fn end_enqueue_failure_still_removes_the_token() {
        let (client, outbound, mut receiver) = make_client();
        let registry = client.progress_registry().clone();
        let begin = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .begin_progress(ProgressOptions::new("Indexing"))
                    .await
            }
        });
        answer_create_ok(&outbound, &mut receiver).await;
        let handle = begin.await.unwrap().unwrap();
        next_notification(&mut receiver).await; // begin

        drop(receiver);
        let error = handle.end(None).unwrap_err();
        assert!(matches!(error, ClientError::OutboundClosed));
        assert_eq!(registry.active_len(), 0, "failed end removed the token");
    }

    #[tokio::test]
    async fn repeated_shared_state_operations_fail_without_sending() {
        let (client, outbound, mut receiver) = make_client();
        let registry = client.progress_registry().clone();
        let begin = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .begin_progress(ProgressOptions::new("Indexing"))
                    .await
            }
        });
        answer_create_ok(&outbound, &mut receiver).await;
        let handle = begin.await.unwrap().unwrap();
        next_notification(&mut receiver).await; // begin

        let shared = handle.shared.clone();
        // Reporting against a token no longer in the registry is an
        // UnknownToken failure and sends nothing.
        registry.remove(&ProgressToken::Int(1));
        let error = shared.report(None, None).unwrap_err();
        assert!(matches!(
            error,
            ClientError::Progress(ProgressError::UnknownToken)
        ));

        // Re-register and end twice through the shared state: only the first
        // end sends.
        assert!(registry.register(ProgressToken::Int(1), false, CancellationToken::new()));
        shared.end(None).unwrap();
        let (method, _) = next_notification(&mut receiver).await;
        assert_eq!(
            method,
            <Progress as crate::types::notification::Notification>::METHOD
        );
        let error = shared.end(None).unwrap_err();
        assert!(matches!(
            error,
            ClientError::Progress(ProgressError::AlreadyEnded)
        ));
        let error = shared.report(None, None).unwrap_err();
        assert!(matches!(
            error,
            ClientError::Progress(ProgressError::AlreadyEnded)
        ));
        assert!(
            receiver.try_recv().is_err(),
            "repeated operations sent nothing"
        );

        // The public handle observes the shared ended state: its drop neither
        // warns nor removes anything.
        drop(handle);
        assert_eq!(registry.active_len(), 0);
    }

    #[tokio::test]
    async fn dropping_an_active_handle_removes_the_token_without_io() {
        let (client, outbound, mut receiver) = make_client();
        let registry = client.progress_registry().clone();
        let begin = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .begin_progress(ProgressOptions::new("Indexing"))
                    .await
            }
        });
        answer_create_ok(&outbound, &mut receiver).await;
        let handle = begin.await.unwrap().unwrap();
        next_notification(&mut receiver).await; // begin
        assert!(registry.is_active(&ProgressToken::Int(1)));

        let _capture = crate::test_util::tracing_capture_lock();
        let events = crate::test_util::EventCapture::new();
        let subscriber = tracing_subscriber::registry().with(events.clone());
        tracing::subscriber::with_default(subscriber, || drop(handle));

        assert_eq!(registry.active_len(), 0, "drop removed the token");
        assert!(
            receiver.try_recv().is_err(),
            "drop performed no I/O and sent no implicit end"
        );
        assert!(
            events.contains("progress handle dropped without end"),
            "drop logged a warning, got {:?}",
            events.messages()
        );
    }

    #[tokio::test]
    async fn concurrent_handles_get_distinct_tokens_and_lifecycles() {
        let (client, outbound, mut receiver) = make_client();
        let begin_a = tokio::spawn({
            let client = client.clone();
            async move { client.begin_progress(ProgressOptions::new("A")).await }
        });
        let begin_b = tokio::spawn({
            let client = client.clone();
            async move { client.begin_progress(ProgressOptions::new("B")).await }
        });

        let params_a = answer_create_ok(&outbound, &mut receiver).await;
        let params_b = answer_create_ok(&outbound, &mut receiver).await;
        assert_eq!(params_a, json!({ "token": 1 }));
        assert_eq!(params_b, json!({ "token": 2 }));

        let handle_a = begin_a.await.unwrap().unwrap();
        let handle_b = begin_b.await.unwrap().unwrap();
        assert_eq!(handle_a.token(), ProgressToken::Int(1));
        assert_eq!(handle_b.token(), ProgressToken::Int(2));

        // Both handles are independent: reporting and ending one does not
        // touch the other's token.
        handle_b.report(None, Some(5)).unwrap();
        handle_a.end(None).unwrap();
        assert!(!client.progress_registry().is_active(&ProgressToken::Int(1)));
        assert!(client.progress_registry().is_active(&ProgressToken::Int(2)));
        handle_b.end(None).unwrap();
    }

    #[tokio::test]
    async fn abandoning_the_begin_future_leaves_no_token_behind() {
        let (client, outbound, mut receiver) = make_client();
        let registry = client.progress_registry().clone();

        let mut begin = Box::pin(client.begin_progress(ProgressOptions::new("Indexing")));
        // Drive the future until the create request is enqueued and pending.
        tokio::select! {
            _ = &mut begin => panic!("begin cannot complete without a response"),
            message = receiver.recv() => {
                let message = message.expect("create request");
                assert!(matches!(message, RawMessage::Request { .. }));
            }
        }
        assert_eq!(outbound.pending_len(), 1);

        // Abandon the future: the pending entry is removed, the peer is told
        // the create was cancelled, and the allocated token never registers.
        drop(begin);
        assert_eq!(outbound.pending_len(), 0);
        let (method, params) = next_notification(&mut receiver).await;
        assert_eq!(
            method,
            <gen_lsp_types::CancelNotification as crate::types::notification::Notification>::METHOD
        );
        assert_eq!(params, json!({ "id": 1 }));
        assert_eq!(
            registry.active_len(),
            0,
            "abandoned begin registered no token"
        );
        assert!(
            receiver.try_recv().is_err(),
            "no begin notification followed"
        );
    }
}
