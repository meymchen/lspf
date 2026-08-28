//! Deterministic, in-memory protocol testing for native consumers.
//!
//! The types in this module exercise the same public [`Transport`]
//! seam as production adapters while keeping peer control and observed wire
//! traffic in the test process.

use std::borrow::Cow;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::runtime::{Runtime, TaskHandle, TaskSend, default_runtime};
use crate::types::{InitializeParams, InitializeResult, InitializedParams, ServerCapabilities};
use crate::{
    BuildError, ClientBuilder, ClientError, Error, Outcome, RawMessage, RequestId, Server,
    ServerHandle, Transport, TransportError, TransportReader, TransportWriter,
};

/// A failure while arranging or driving a reusable protocol journey.
#[derive(Debug, thiserror::Error)]
pub enum JourneyError {
    /// Static Client or Server registrations were invalid.
    #[error(transparent)]
    Build(#[from] BuildError),
    /// The endpoint failed while connecting or serving.
    #[error(transparent)]
    Endpoint(#[from] Error),
    /// A typed Client lifecycle operation failed.
    #[error(transparent)]
    Client(#[from] ClientError),
    /// The in-memory Transport closed or failed.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// A journey-owned task stopped before publishing its result.
    #[error("journey task stopped before publishing its result")]
    TaskStopped,
    /// A standard lifecycle value could not be encoded.
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
    /// The endpoint emitted a message other than the required lifecycle step.
    #[error("expected {expected}, received {actual}")]
    UnexpectedMessage {
        /// Lifecycle message the journey was waiting for.
        expected: &'static str,
        /// Debug representation of the message that arrived.
        actual: String,
    },
}

/// Guard for Tokio's deterministic, paused clock.
///
/// Create this only inside a current-thread Tokio runtime. Dropping the guard
/// resumes wall-clock time. Tokio panics if the runtime clock is already
/// paused, which prevents overlapping guards from silently sharing state.
#[derive(Debug)]
pub struct VirtualClock {
    paused: bool,
}

impl VirtualClock {
    /// Pause the current Tokio runtime's clock.
    pub fn pause() -> Self {
        tokio::time::pause();
        Self { paused: true }
    }

    /// Move the paused clock forward and let newly ready deadline tasks run.
    pub async fn advance(&self, duration: Duration) {
        tokio::time::advance(duration).await;
        tokio::task::yield_now().await;
    }

    /// Resume wall-clock time before this guard would otherwise be dropped.
    pub fn resume(mut self) {
        tokio::time::resume();
        self.paused = false;
    }
}

impl Drop for VirtualClock {
    fn drop(&mut self) {
        if self.paused {
            tokio::time::resume();
        }
    }
}

/// Direction of one captured message relative to the endpoint under test.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireDirection {
    /// The scripted peer delivered a message to the endpoint.
    PeerToEndpoint,
    /// The endpoint delivered a message to the scripted peer.
    EndpointToPeer,
}

/// One message observed at the in-memory Transport seam.
#[derive(Clone, Debug)]
pub struct WireEvent {
    sequence: u64,
    direction: WireDirection,
    message: RawMessage,
}

impl WireEvent {
    /// Monotonic, zero-based position in this capture.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Direction in which the message crossed the Transport seam.
    pub fn direction(&self) -> WireDirection {
        self.direction
    }

    /// Captured JSON-RPC message.
    pub fn message(&self) -> &RawMessage {
        &self.message
    }
}

/// Cloneable view of all traffic observed by one in-memory connection.
#[derive(Clone, Debug)]
pub struct WireCapture {
    events: Option<Arc<Mutex<Vec<WireEvent>>>>,
}

impl WireCapture {
    /// Return a stable snapshot in the order messages crossed the seam.
    pub fn snapshot(&self) -> Vec<WireEvent> {
        self.events
            .as_ref()
            .map(|events| events.lock().unwrap().clone())
            .unwrap_or_default()
    }

    fn record(events: &mut Vec<WireEvent>, direction: WireDirection, message: RawMessage) {
        let sequence = events.len() as u64;
        events.push(WireEvent {
            sequence,
            direction,
            message,
        });
    }

    fn deliver<E>(
        &self,
        direction: WireDirection,
        message: RawMessage,
        deliver: impl FnOnce(RawMessage) -> Result<(), E>,
    ) -> Result<(), TransportError> {
        let Some(events) = &self.events else {
            return deliver(message).map_err(|_| TransportError::Closed);
        };
        let mut events = events.lock().unwrap();
        deliver(message.clone()).map_err(|_| TransportError::Closed)?;
        Self::record(&mut events, direction, message);
        Ok(())
    }
}

/// In-memory message-framed Transport controlled by a [`ScriptedPeer`].
pub struct MemoryTransport {
    incoming: mpsc::UnboundedReceiver<Result<RawMessage, TransportError>>,
    outgoing: mpsc::UnboundedSender<RawMessage>,
    capture: WireCapture,
}

impl MemoryTransport {
    /// Create one endpoint Transport and its controlling peer.
    pub fn pair() -> (Self, ScriptedPeer) {
        Self::pair_with_capture(WireCapture {
            events: Some(Arc::new(Mutex::new(Vec::new()))),
        })
    }

    /// Create an in-memory Transport that forwards messages without retaining
    /// wire history.
    ///
    /// Prefer this for long-lived or high-volume workloads where cloning every
    /// message into a [`WireCapture`] would make the test harness itself grow
    /// with total traffic. [`ScriptedPeer::capture`] remains available for a
    /// uniform interface, but its snapshot stays empty for this pair.
    pub fn pair_uncaptured() -> (Self, ScriptedPeer) {
        Self::pair_with_capture(WireCapture { events: None })
    }

    fn pair_with_capture(capture: WireCapture) -> (Self, ScriptedPeer) {
        let (to_endpoint, incoming) = mpsc::unbounded_channel();
        let (outgoing, from_endpoint) = mpsc::unbounded_channel();
        (
            Self {
                incoming,
                outgoing,
                capture: capture.clone(),
            },
            ScriptedPeer {
                to_endpoint,
                from_endpoint,
                capture,
            },
        )
    }
}

/// Read half of a [`MemoryTransport`].
pub struct MemoryReader(mpsc::UnboundedReceiver<Result<RawMessage, TransportError>>);

/// Write half of a [`MemoryTransport`].
pub struct MemoryWriter {
    outgoing: mpsc::UnboundedSender<RawMessage>,
    capture: WireCapture,
}

impl Transport for MemoryTransport {
    type Reader = MemoryReader;
    type Writer = MemoryWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            MemoryReader(self.incoming),
            MemoryWriter {
                outgoing: self.outgoing,
                capture: self.capture,
            },
        )
    }
}

impl TransportReader for MemoryReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.0.recv().await.unwrap_or(Err(TransportError::Closed))
    }
}

impl TransportWriter for MemoryWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        self.capture
            .deliver(WireDirection::EndpointToPeer, message, |message| {
                self.outgoing.send(message)
            })
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

/// Peer-side controller for scripting input and observing endpoint output.
pub struct ScriptedPeer {
    to_endpoint: mpsc::UnboundedSender<Result<RawMessage, TransportError>>,
    from_endpoint: mpsc::UnboundedReceiver<RawMessage>,
    capture: WireCapture,
}

impl ScriptedPeer {
    /// Clone the ordered capture for this connection.
    pub fn capture(&self) -> WireCapture {
        self.capture.clone()
    }

    /// Deliver one message to the endpoint.
    pub fn send(&self, message: RawMessage) -> Result<(), TransportError> {
        self.capture
            .deliver(WireDirection::PeerToEndpoint, message, |message| {
                self.to_endpoint.send(Ok(message))
            })
    }

    /// Make the endpoint's next read fail with a scripted Transport error.
    pub fn fail(&self, error: TransportError) -> Result<(), TransportError> {
        self.to_endpoint
            .send(Err(error))
            .map_err(|_| TransportError::Closed)
    }

    /// Receive the next message emitted by the endpoint.
    pub async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.from_endpoint
            .recv()
            .await
            .ok_or(TransportError::Closed)
    }
}

/// A running Server connected to a scripted in-memory client peer.
pub struct ServerJourney {
    peer: ScriptedPeer,
    serving: JourneyTask<crate::Result<Outcome>>,
}

impl ServerJourney {
    /// Initialize a Server with default LSP client parameters.
    pub async fn start<S>(server: Server<S>) -> Result<Self, JourneyError>
    where
        S: Send + Sync + 'static,
    {
        Self::start_with(server, InitializeParams::default()).await
    }

    /// Initialize a Server with caller-supplied LSP client parameters.
    pub async fn start_with<S>(
        server: Server<S>,
        params: InitializeParams,
    ) -> Result<Self, JourneyError>
    where
        S: Send + Sync + 'static,
    {
        let (transport, mut peer) = MemoryTransport::pair();
        let serving = spawn_journey(server.serve(transport));
        let initialize_id = RequestId::Number(1);
        peer.send(request(initialize_id.clone(), "initialize", &params)?)?;
        expect_success_response(&mut peer, &initialize_id, "initialize response").await?;
        peer.send(notification("initialized", &InitializedParams {})?)?;
        Ok(Self { peer, serving })
    }

    /// Access the scripted client peer for calls beyond the standard lifecycle.
    pub fn peer(&mut self) -> &mut ScriptedPeer {
        &mut self.peer
    }

    /// Clone the complete ordered wire capture for this journey.
    pub fn capture(&self) -> WireCapture {
        self.peer.capture()
    }

    /// Run shutdown and exit, then return the Server's terminal outcome.
    pub async fn finish(mut self) -> Result<Outcome, JourneyError> {
        let shutdown_id = RequestId::Number(2);
        self.peer.send(request(
            shutdown_id.clone(),
            "shutdown",
            &serde_json::Value::Null,
        )?)?;
        expect_success_response(&mut self.peer, &shutdown_id, "shutdown response").await?;
        self.peer
            .send(notification("exit", &serde_json::Value::Null)?)?;
        Ok(self.serving.join().await??)
    }
}

/// A running Client connected to a scripted in-memory server peer.
pub struct ClientJourney {
    peer: ScriptedPeer,
    server: ServerHandle,
    serving: JourneyTask<crate::Result<Outcome>>,
}

impl ClientJourney {
    /// Build and initialize a Client against a default-capability server peer.
    pub async fn start(builder: ClientBuilder) -> Result<Self, JourneyError> {
        Self::start_with(
            builder,
            InitializeResult {
                capabilities: ServerCapabilities::default(),
                server_info: None,
            },
        )
        .await
    }

    /// Build and initialize a Client against a caller-supplied initialize result.
    pub async fn start_with(
        builder: ClientBuilder,
        initialize_result: InitializeResult,
    ) -> Result<Self, JourneyError> {
        let (transport, mut peer) = MemoryTransport::pair();
        let client = builder.build(transport)?;
        let connecting = spawn_journey(client.connect());
        let initialize_id = expect_request(&mut peer, "initialize").await?;
        peer.send(response(initialize_id, &initialize_result)?)?;
        let connection = connecting.join().await??;
        let server = connection.server();
        let serving = spawn_journey(connection.serve());
        expect_notification(&mut peer, "initialized").await?;
        Ok(Self {
            peer,
            server,
            serving,
        })
    }

    /// Clone the typed handle used for client-to-server calls.
    pub fn server(&self) -> ServerHandle {
        self.server.clone()
    }

    /// Access the scripted server peer for calls beyond the standard lifecycle.
    pub fn peer(&mut self) -> &mut ScriptedPeer {
        &mut self.peer
    }

    /// Clone the complete ordered wire capture for this journey.
    pub fn capture(&self) -> WireCapture {
        self.peer.capture()
    }

    /// Run shutdown and exit, then return the Client's terminal outcome.
    pub async fn finish(mut self) -> Result<Outcome, JourneyError> {
        let shutdown = spawn_journey({
            let server = self.server.clone();
            async move { server.shutdown().await }
        });
        let shutdown_id = expect_request(&mut self.peer, "shutdown").await?;
        self.peer
            .send(response(shutdown_id, &serde_json::Value::Null)?)?;
        shutdown.join().await??;
        self.server.exit()?;
        expect_notification(&mut self.peer, "exit").await?;
        Ok(self.serving.join().await??)
    }
}

struct JourneyTask<T> {
    task: TaskHandle,
    output: futures_channel::oneshot::Receiver<T>,
}

impl<T> JourneyTask<T> {
    async fn join(self) -> Result<T, JourneyError> {
        let output = self.output.await;
        self.task.join().await;
        output.map_err(|_| JourneyError::TaskStopped)
    }
}

fn spawn_journey<T, F>(future: F) -> JourneyTask<T>
where
    T: Send + 'static,
    F: Future<Output = T> + TaskSend + 'static,
{
    let (output_tx, output) = futures_channel::oneshot::channel();
    let task = default_runtime().spawn(async move {
        let result = future.await;
        let _ = output_tx.send(result);
    });
    JourneyTask { task, output }
}

fn request(
    id: RequestId,
    method: &'static str,
    params: &impl Serialize,
) -> Result<RawMessage, serde_json::Error> {
    Ok(RawMessage::Request {
        id,
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(params)?),
    })
}

fn notification(
    method: &'static str,
    params: &impl Serialize,
) -> Result<RawMessage, serde_json::Error> {
    Ok(RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(params)?),
    })
}

fn response(id: RequestId, result: &impl Serialize) -> Result<RawMessage, serde_json::Error> {
    Ok(RawMessage::Response {
        id,
        result: Ok(Bytes::from(serde_json::to_vec(result)?)),
    })
}

async fn expect_request(
    peer: &mut ScriptedPeer,
    expected_method: &'static str,
) -> Result<RequestId, JourneyError> {
    let message = peer.recv().await?;
    match message {
        RawMessage::Request { id, method, .. } if method == expected_method => Ok(id),
        other => Err(JourneyError::UnexpectedMessage {
            expected: expected_method,
            actual: format!("{other:?}"),
        }),
    }
}

async fn expect_notification(
    peer: &mut ScriptedPeer,
    expected_method: &'static str,
) -> Result<(), JourneyError> {
    let message = peer.recv().await?;
    match message {
        RawMessage::Notification { method, .. } if method == expected_method => Ok(()),
        other => Err(JourneyError::UnexpectedMessage {
            expected: expected_method,
            actual: format!("{other:?}"),
        }),
    }
}

async fn expect_success_response(
    peer: &mut ScriptedPeer,
    expected_id: &RequestId,
    expected: &'static str,
) -> Result<Bytes, JourneyError> {
    let message = peer.recv().await?;
    match message {
        RawMessage::Response {
            id,
            result: Ok(body),
        } if &id == expected_id => Ok(body),
        other => Err(JourneyError::UnexpectedMessage {
            expected,
            actual: format!("{other:?}"),
        }),
    }
}
