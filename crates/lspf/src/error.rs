use std::fmt::Display;

use thiserror::Error;

use crate::transport::TransportError;

/// Result type returned by serving and Transport entry points.
pub type Result<T> = std::result::Result<T, Error>;

/// A terminal framework or Transport failure while serving a connection.
#[derive(Debug, Error)]
pub enum Error {
    /// A protocol error produced while handling an incoming call.
    #[error(transparent)]
    Lsp(#[from] LspError),

    /// The underlying Transport failed.
    #[error(transparent)]
    Transport(#[from] TransportError),

    /// Native serving was attempted without a running Tokio runtime. The
    /// framework never starts a runtime implicitly (ADR 0020): start one —
    /// for example with `#[tokio::main]` — before serving the connection.
    #[cfg(not(target_arch = "wasm32"))]
    #[error("no Tokio runtime is running; start one before serving the connection")]
    RuntimeRequired,
}

/// A failure to enqueue a typed server-to-client operation.
#[derive(Debug, Error)]
pub enum ClientError {
    /// The typed parameters could not be encoded as JSON.
    #[error("failed to serialize client parameters: {0}")]
    Serialize(#[source] serde_json::Error),

    /// The connection began closing before the operation could be enqueued.
    #[error("client connection is closed")]
    ConnectionClosed,

    /// The engine-owned outbound queue is closed.
    #[error("client outbound queue is closed")]
    OutboundClosed,

    /// The session was cancelled before the peer answered the request.
    #[error("client request was cancelled")]
    Cancelled,

    /// The connection exhausted the positive outbound request-ID or
    /// progress-token space; no further server-to-client requests or progress
    /// tokens can be allocated on this connection.
    #[error("outbound request ID space exhausted")]
    IdExhausted,

    /// The remote peer returned a JSON-RPC error response. The original
    /// error's code, message, and optional data are preserved.
    #[error("remote error (code {code}): {message}", code = .0.code, message = .0.message)]
    Remote(crate::raw::JsonRpcError),

    /// The remote peer returned a success result that could not be decoded
    /// into the expected type.
    #[error("failed to deserialize client response: {0}")]
    Deserialize(#[source] serde_json::Error),

    /// A named helper rejected its own parameters before anything was
    /// encoded or enqueued: no request or notification was sent.
    #[error("invalid helper parameters: {0}")]
    InvalidHelperParams(String),

    /// A work-done progress lifecycle failure from a
    /// [`ProgressHandle`](crate::ProgressHandle) operation.
    #[error(transparent)]
    Progress(#[from] ProgressError),
}

/// A failure of the connection-scoped work-done progress lifecycle
/// ([`Client::begin_progress`](crate::Client::begin_progress) and the
/// resulting [`ProgressHandle`](crate::ProgressHandle)).
///
/// These failures describe the handle's own state, not transport or remote
/// failures: enqueue and response failures surface as the other
/// [`ClientError`] variants.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProgressError {
    /// The handle's progress was already ended; an ended handle reports and
    /// ends nothing further.
    #[error("progress handle has already ended")]
    AlreadyEnded,

    /// The handle's cancellation token was cancelled; a cancelled handle
    /// sends no further report notifications.
    #[error("progress was cancelled")]
    Cancelled,

    /// The handle's token is not active on this connection anymore (for
    /// example because the handle was dropped or the registry entry was
    /// removed by another path).
    #[error("progress token is not active on this connection")]
    UnknownToken,

    /// A report percentage outside the inclusive range 0 through 100.
    #[error("progress percentage {0} is outside the range 0..=100")]
    InvalidPercentage(u32),
}

/// A configuration error surfaced by [`ServerBuilder::build`](crate::ServerBuilder::build).
///
/// `BuildError` is deliberately distinct from [`LspError`]: it names a static
/// registration mistake the developer must fix before the server ever runs,
/// never a value that goes on the wire. `build()` performs no I/O and returns
/// this before any transport is touched.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BuildError {
    /// Two handlers were registered for the same request method.
    #[error("duplicate handler registered for method `{0}`")]
    DuplicateMethod(String),

    /// A custom request or notification was registered under a method the
    /// framework owns (`initialize`, `shutdown`, `exit`, `initialized`,
    /// `$/cancelRequest`).
    #[error("method `{0}` is reserved by the framework and cannot be overridden")]
    ReservedMethod(String),

    /// Two handlers were registered for the same command name.
    #[error("duplicate handler registered for command `{0}`")]
    DuplicateCommand(String),

    /// A command was registered with an empty name; a command name is the key
    /// the editor sends on `workspace/executeCommand`, so it cannot be empty.
    #[error("a command name cannot be empty")]
    EmptyCommandName,

    /// One or more commands were registered alongside an explicit
    /// `workspace/executeCommand` request handler. Commands already dispatch
    /// beneath that method, so a user handler for it would shadow them.
    #[error(
        "commands conflict with an explicit `workspace/executeCommand` handler; \
         register either commands or the raw method, not both"
    )]
    ExecuteCommandConflict,

    /// Two contributions disagreed on a capability field that must be singular
    /// within its family, or a dependent feature was registered without the
    /// base its family requires (ADR 0017). Capability construction never
    /// resolves such a clash by last-write-wins; it fails the build instead.
    #[error("conflicting contributions for capability field `{field}`")]
    ConflictingCapability {
        /// The singular `ServerCapabilities` field with conflicting inputs.
        field: &'static str,
    },

    /// `configure_initialize` was supplied more than once. There is exactly one
    /// initialization-dependent registration transaction (ADR 0017).
    #[error("`configure_initialize` may only be supplied once")]
    DuplicateConfigureInitialize,

    /// A lifecycle hook (`on_initialize`, …) was supplied more than once. Each
    /// hook has at most one registration (ADR 0018).
    #[error("lifecycle hook `{0}` may only be supplied once")]
    DuplicateLifecycleHook(&'static str),

    /// A zero concurrency limit could never admit a user call.
    #[error("concurrency limit must be greater than zero")]
    InvalidConcurrencyLimit,

    /// A zero outbound warning threshold could never be crossed from below.
    #[error("outbound warning threshold must be greater than zero")]
    InvalidOutboundWarningThreshold,

    /// A connection resource budget or enabled deadline was zero.
    #[error("resource policy `{field}` must be greater than zero when enabled")]
    InvalidResourcePolicy {
        /// The invalid [`ResourcePolicy`](crate::ResourcePolicy) field.
        field: crate::ResourcePolicyField,
    },
}

/// A JSON-RPC/LSP error suitable for returning from a handler.
#[derive(Debug, Error)]
pub enum LspError {
    /// An unexpected server failure (`-32603`).
    #[error("internal error: {0}")]
    Internal(String),

    /// Invalid method parameters (`-32602`).
    #[error("invalid params: {0}")]
    InvalidParams(String),

    /// No handler exists for the method (`-32601`).
    #[error("method not found: {0}")]
    MethodNotFound(String),

    /// The peer cancelled the request (`-32800`).
    #[error("request cancelled")]
    RequestCancelled,

    /// The requested content changed before completion (`-32801`).
    #[error("content modified")]
    ContentModified,

    /// A request arrived before initialization completed (`-32002`).
    #[error("server not initialized")]
    ServerNotInitialized,

    /// The incoming JSON-RPC request is invalid (`-32600`).
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// An application-defined server error.
    #[error("{message}")]
    ServerError {
        /// Application-defined JSON-RPC error code.
        code: i32,
        /// Human-readable error message.
        message: String,
        /// Optional structured error data.
        data: Option<serde_json::Value>,
    },
}

impl LspError {
    /// Construct an internal-error response from a displayable value.
    pub fn internal(e: impl Display) -> Self {
        Self::Internal(e.to_string())
    }

    /// Construct an invalid-params response from a displayable value.
    pub fn invalid_params(e: impl Display) -> Self {
        Self::InvalidParams(e.to_string())
    }

    /// Construct an invalid-request response from a displayable value.
    pub fn invalid_request(e: impl Display) -> Self {
        Self::InvalidRequest(e.to_string())
    }

    /// The JSON-RPC/LSP numeric error code.
    pub fn code(&self) -> i32 {
        match self {
            Self::Internal(_) => -32603,
            Self::InvalidParams(_) => -32602,
            Self::MethodNotFound(_) => -32601,
            Self::RequestCancelled => -32800,
            Self::ContentModified => -32801,
            Self::ServerNotInitialized => -32002,
            Self::InvalidRequest(_) => -32600,
            Self::ServerError { code, .. } => *code,
        }
    }

    /// The human-readable error message.
    pub fn message(&self) -> String {
        match self {
            Self::Internal(m)
            | Self::InvalidParams(m)
            | Self::MethodNotFound(m)
            | Self::InvalidRequest(m) => m.clone(),
            Self::RequestCancelled => "request cancelled".to_string(),
            Self::ContentModified => "content modified".to_string(),
            Self::ServerNotInitialized => "server not initialized".to_string(),
            Self::ServerError { message, .. } => message.clone(),
        }
    }

    /// Optional structured error data for an application-defined server error.
    pub fn data(&self) -> Option<&serde_json::Value> {
        match self {
            Self::ServerError { data, .. } => data.as_ref(),
            _ => None,
        }
    }
}
