use std::borrow::Cow;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// A JSON-RPC request identifier accepted by LSP.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// Numeric request identifier.
    Number(i32),
    /// String request identifier.
    String(String),
}

/// A validated JSON-RPC envelope with parameters and results retained as raw
/// UTF-8 JSON bytes.
#[derive(Debug, Clone)]
pub enum RawMessage {
    /// A request that expects a response.
    Request {
        /// Request identifier to echo in the response.
        id: RequestId,
        /// LSP or custom method name.
        method: Cow<'static, str>,
        /// Raw JSON parameters.
        params: Bytes,
    },
    /// A notification that expects no response.
    Notification {
        /// LSP or custom method name.
        method: Cow<'static, str>,
        /// Raw JSON parameters.
        params: Bytes,
    },
    /// A success or error response to an earlier request.
    Response {
        /// Identifier of the request being answered.
        id: RequestId,
        /// Raw JSON result or a structured JSON-RPC error.
        result: std::result::Result<Bytes, JsonRpcError>,
    },
    /// A JSON-RPC parse or envelope-validation error. Serializes as an
    /// error response with a null ID because no ordinary request ID is safe
    /// to echo (JSON-RPC 2.0 §5).
    ProtocolError {
        /// Error to serialize with a null request ID.
        error: JsonRpcError,
    },
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Numeric JSON-RPC or LSP error code.
    pub code: i32,
    /// Human-readable error message.
    pub message: String,
    /// Optional structured application data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl RawMessage {
    /// The method name for requests and notifications.
    pub fn method(&self) -> Option<&str> {
        match self {
            Self::Request { method, .. } | Self::Notification { method, .. } => Some(method),
            Self::Response { .. } | Self::ProtocolError { .. } => None,
        }
    }

    /// The request identifier for requests and responses.
    pub fn id(&self) -> Option<&RequestId> {
        match self {
            Self::Request { id, .. } | Self::Response { id, .. } => Some(id),
            Self::Notification { .. } | Self::ProtocolError { .. } => None,
        }
    }
}
