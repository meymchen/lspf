use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use lsp_types::notification::Notification;
use lsp_types::request::Request;
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use tokio_util::sync::CancellationToken;

use crate::error::ClientError;
use crate::raw::{JsonRpcError, RawMessage, RequestId};

/// A response completion value: either raw success bytes or a JSON-RPC error.
type PendingResult = std::result::Result<Bytes, JsonRpcError>;

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
    pending: HashMap<u32, oneshot::Sender<PendingResult>>,
    /// Monotonically increasing counter; the next ID to try allocating.
    next_id: u32,
}

impl Default for OutboundInner {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
            next_id: 1,
        }
    }
}

/// RAII guard that removes a pending outbound-request entry from the registry
/// when dropped. Guards against cancelled or failed `request()` futures that
/// would otherwise leak entries in the pending map indefinitely.
struct PendingGuard {
    registry: OutboundRegistry,
    id: u32,
}

impl PendingGuard {
    fn new(registry: OutboundRegistry, id: u32) -> Self {
        Self { registry, id }
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        // Silently no-ops if the entry was already removed by `complete()`.
        self.registry.remove(self.id);
    }
}

impl OutboundRegistry {
    /// Allocate the next available outbound request ID (starting at 1,
    /// wrapping around while scanning for an unused slot) and store the
    /// completion sender.
    ///
    /// Returns the allocated ID and the receiver the caller should await.
    pub(crate) fn insert(&self) -> (u32, oneshot::Receiver<PendingResult>) {
        let (tx, rx) = oneshot::channel();
        let mut inner = self.inner.lock().unwrap();
        // Scan from `next_id` upward (with wraparound at u32::MAX back to 1)
        // until we find a slot that is not currently in use.
        let id = loop {
            let candidate = inner.next_id;
            // Advance the counter; skip 0 so IDs stay positive.
            inner.next_id = inner.next_id.wrapping_add(1).max(1);
            if !inner.pending.contains_key(&candidate) {
                break candidate;
            }
        };
        inner.pending.insert(id, tx);
        (id, rx)
    }

    /// Remove and complete the pending entry for `id` with `result`.
    ///
    /// If no entry exists (unknown, duplicate, or late response), returns
    /// `false` and leaves all other entries intact.
    pub(crate) fn complete(&self, id: u32, result: PendingResult) -> bool {
        let tx = self.inner.lock().unwrap().pending.remove(&id);
        if let Some(tx) = tx {
            // Receiver may be gone if the caller was cancelled; ignore.
            let _ = tx.send(result);
            true
        } else {
            false
        }
    }

    /// Remove the pending entry for `id` without completing it.
    ///
    /// Used when encoding or enqueue fails immediately after `insert`.
    pub(crate) fn remove(&self, id: u32) -> Option<oneshot::Sender<PendingResult>> {
        self.inner.lock().unwrap().pending.remove(&id)
    }

    /// Complete every remaining pending entry with a session-closed error,
    /// then clear the registry.
    pub(crate) fn close_all(&self) {
        let entries: HashMap<u32, oneshot::Sender<PendingResult>> =
            std::mem::take(&mut self.inner.lock().unwrap().pending);
        for tx in entries.into_values() {
            let _ = tx.send(Err(JsonRpcError {
                code: -32099,
                message: "session closed".to_string(),
                data: None,
            }));
        }
    }

    /// Override the next candidate ID (test-only, for wraparound coverage).
    #[cfg(test)]
    pub(crate) fn set_next_id(&self, id: u32) {
        self.inner.lock().unwrap().next_id = id;
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

    /// Encode and enqueue one typed server-to-client request, then await the
    /// correlated response.
    ///
    /// The method allocates a unique outbound ID, inserts the pending decoder
    /// before enqueue, and returns a `Future` that resolves once the engine
    /// delivers the matching response. Responses may arrive in any order.
    ///
    /// # Errors
    ///
    /// - [`ClientError::ConnectionClosed`] or [`ClientError::OutboundClosed`]
    ///   if the session is already closing.
    /// - [`ClientError::Serialize`] if the params cannot be encoded.
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

        // 3. Allocate an ID and insert the pending entry.
        let (id, rx) = self.outbound.insert();
        // Guard ensures the pending entry is removed if this future is dropped
        // before a response arrives (e.g. due to caller cancellation).
        let _guard = PendingGuard::new(self.outbound.clone(), id);
        let request_id = RequestId::Number(id as i32);

        // 4. Enqueue.  On any failure remove and complete the pending entry.
        let message = RawMessage::Request {
            id: request_id,
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

        // 5. Await the response.
        let raw = rx.await.map_err(|_| ClientError::OutboundClosed)?;

        match raw {
            Ok(bytes) => {
                serde_json::from_slice::<R::Result>(&bytes).map_err(ClientError::Deserialize)
            }
            Err(e) => Err(ClientError::Remote {
                code: e.code,
                message: e.message,
            }),
        }
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
    fn outbound_ids_start_at_1_and_increase_monotonically() {
        let registry = OutboundRegistry::default();
        let (id1, _rx1) = registry.insert();
        let (id2, _rx2) = registry.insert();
        let (id3, _rx3) = registry.insert();
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[test]
    fn complete_returns_true_for_known_id_and_false_for_unknown() {
        let registry = OutboundRegistry::default();
        let (id, mut rx) = registry.insert();
        assert!(registry.complete(id, Ok(Bytes::from_static(b"null"))));
        // Now gone — second call returns false.
        assert!(!registry.complete(id, Ok(Bytes::from_static(b"null"))));
        // Unknown ID also returns false.
        assert!(!registry.complete(999, Ok(Bytes::from_static(b"null"))));
        // The receiver should have received the value.
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn close_all_completes_pending_with_session_closed_error() {
        let registry = OutboundRegistry::default();
        let (_id1, mut rx1) = registry.insert();
        let (_id2, mut rx2) = registry.insert();
        registry.close_all();
        let r1 = rx1.try_recv().unwrap();
        let r2 = rx2.try_recv().unwrap();
        assert!(r1.is_err());
        assert!(r2.is_err());
    }

    #[test]
    fn allocator_wraparound_skips_in_use_ids() {
        let registry = OutboundRegistry::default();
        // Pre-populate id 1 with a pending entry by hand to simulate wraparound.
        let (id1, _rx1) = registry.insert(); // id = 1
        assert_eq!(id1, 1);
        // Force next_id back to 1 to test wraparound scan.
        registry.set_next_id(1);
        let (id2, _rx2) = registry.insert(); // should skip 1, use 2
        assert_eq!(id2, 2);
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
                data: None,
            }),
        );

        let err = handle.await.unwrap().unwrap_err();
        assert!(matches!(err, ClientError::Remote { code: -32000, .. }));
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
        let handle =
            tokio::spawn(async move { client2.request::<PingRequest>(json!({})).await });

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
    }
}
