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

/// Non-sensitive identity attached to a connection failure when available.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectionFailureContext {
    /// Monotonic process-local identity of the affected connection.
    pub connection_id: u64,
    /// Traffic direction, when the failure belongs to one side of the wire.
    pub direction: Option<ConnectionDirection>,
    /// LSP or custom method, when known without inspecting a payload.
    pub method: Option<String>,
    /// JSON-RPC request ID, when the event belongs to a request.
    pub request_id: Option<RequestId>,
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
                request_id: request_id.cloned(),
            },
        };
        if catch_unwind(AssertUnwindSafe(|| hook.invoke((failure,)))).is_err() {
            error!("panic isolated while invoking connection error hook");
        }
    }
}
