use std::fmt::Display;

use thiserror::Error;

use crate::transport::TransportError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Lsp(#[from] LspError),

    #[error(transparent)]
    Transport(#[from] TransportError),
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

    /// A custom request was registered under a method the framework owns
    /// (`initialize`, `shutdown`, `exit`, `initialized`, `$/cancelRequest`).
    #[error("method `{0}` is reserved by the framework and cannot be overridden")]
    ReservedMethod(String),
}

#[derive(Debug, Error)]
pub enum LspError {
    #[error("internal error: {0}")]
    Internal(String),

    #[error("invalid params: {0}")]
    InvalidParams(String),

    #[error("method not found: {0}")]
    MethodNotFound(String),

    #[error("request cancelled")]
    RequestCancelled,

    #[error("content modified")]
    ContentModified,

    #[error("server not initialized")]
    ServerNotInitialized,

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("{message}")]
    ServerError {
        code: i32,
        message: String,
        data: Option<serde_json::Value>,
    },
}

impl LspError {
    pub fn internal(e: impl Display) -> Self {
        Self::Internal(e.to_string())
    }

    pub fn invalid_params(e: impl Display) -> Self {
        Self::InvalidParams(e.to_string())
    }

    pub fn invalid_request(e: impl Display) -> Self {
        Self::InvalidRequest(e.to_string())
    }

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

    pub fn data(&self) -> Option<&serde_json::Value> {
        match self {
            Self::ServerError { data, .. } => data.as_ref(),
            _ => None,
        }
    }
}
