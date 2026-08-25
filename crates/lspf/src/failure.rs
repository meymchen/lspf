use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use tracing::error;

use crate::RequestId;
use crate::builder::SharedHandler;

/// A stable class of connection-level failure observed by an error hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionFailureCategory {
    /// The Transport could not recover an accepted message frame.
    Framing,
    /// A decoded JSON-RPC message violated the protocol contract.
    Protocol,
    /// A Transport read or write operation failed.
    Transport,
    /// Framework panic isolation caught a panic from user dispatch.
    PanicIsolation,
    /// A connection-owned resource budget rejected work.
    Overload,
    /// Final Transport shutdown failed while the connection was closing.
    Close,
}

/// The direction in which a reported connection failure occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionDirection {
    /// Traffic received from the client.
    Inbound,
    /// Traffic sent to the client.
    Outbound,
}

/// Non-sensitive identity for a JSON-RPC request.
///
/// Numeric IDs retain their value for correlation. String IDs are represented
/// only by their kind because their peer-controlled contents may be sensitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionRequestId {
    /// A numeric JSON-RPC request ID.
    Number(i32),
    /// A string JSON-RPC request ID, with its contents redacted.
    String,
}

impl From<&RequestId> for ConnectionRequestId {
    fn from(value: &RequestId) -> Self {
        match value {
            RequestId::Number(number) => Self::Number(*number),
            RequestId::String(_) => Self::String,
        }
    }
}

/// Non-sensitive identity attached to a connection failure when available.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectionFailureContext {
    /// Monotonic process-local identity of the affected connection.
    pub connection_id: u64,
    /// Traffic direction, when the failure belongs to one side of the wire.
    pub direction: Option<ConnectionDirection>,
    /// Framework-owned, registered, or locally declared typed outbound method,
    /// when known without trusting an unvalidated peer-controlled method name.
    pub method: Option<String>,
    /// JSON-RPC request ID, when the event belongs to a request.
    pub request_id: Option<ConnectionRequestId>,
}

/// One connection-level failure delivered to [`ServerBuilder::on_error`](crate::ServerBuilder::on_error).
///
/// The report deliberately excludes parameters, results, document text, wire
/// payloads, panic payloads, and underlying error messages.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectionFailure {
    /// Stable category suitable for metrics and policy decisions.
    pub category: ConnectionFailureCategory,
    /// Non-sensitive connection and call identity.
    pub context: ConnectionFailureContext,
}

pub(crate) type ErrorHook = Arc<dyn SharedHandler<(ConnectionFailure,), ()>>;

#[derive(Clone)]
pub(crate) struct FailureReporter {
    hook: Option<ErrorHook>,
    connection_id: u64,
}

impl FailureReporter {
    pub(crate) fn new(hook: Option<ErrorHook>, connection_id: u64) -> Self {
        Self {
            hook,
            connection_id,
        }
    }

    pub(crate) fn report(
        &self,
        category: ConnectionFailureCategory,
        direction: Option<ConnectionDirection>,
        method: Option<&str>,
        request_id: Option<&RequestId>,
    ) {
        let Some(hook) = &self.hook else { return };
        let failure = ConnectionFailure {
            category,
            context: ConnectionFailureContext {
                connection_id: self.connection_id,
                direction,
                method: method.map(str::to_owned),
                request_id: request_id.map(ConnectionRequestId::from),
            },
        };
        if catch_unwind(AssertUnwindSafe(|| hook.invoke((failure,)))).is_err() {
            error!("panic isolated while invoking connection error hook");
        }
    }

    /// Report an inbound event whose method name has not been matched against
    /// a framework-owned or registered method. Peer-controlled method names
    /// are payload-adjacent data, so they are deliberately omitted.
    pub(crate) fn report_unvalidated_inbound_method(
        &self,
        category: ConnectionFailureCategory,
        request_id: Option<&RequestId>,
    ) {
        self.report(
            category,
            Some(ConnectionDirection::Inbound),
            None,
            request_id,
        );
    }
}
