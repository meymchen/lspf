use std::borrow::Cow;

use bytes::Bytes;
use lsp_types::NumberOrString;
use serde::{Deserialize, Serialize};

pub type RequestId = NumberOrString;

#[derive(Debug, Clone)]
pub enum RawMessage {
    Request {
        id: RequestId,
        method: Cow<'static, str>,
        params: Bytes,
    },
    Notification {
        method: Cow<'static, str>,
        params: Bytes,
    },
    Response {
        id: RequestId,
        result: std::result::Result<Bytes, JsonRpcError>,
    },
    /// A JSON-RPC parse or envelope-validation error. Serializes as an
    /// error response with a null ID because no ordinary request ID is safe
    /// to echo (JSON-RPC 2.0 §5).
    ProtocolError { error: JsonRpcError },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl RawMessage {
    pub fn method(&self) -> Option<&str> {
        match self {
            Self::Request { method, .. } | Self::Notification { method, .. } => Some(method),
            Self::Response { .. } | Self::ProtocolError { .. } => None,
        }
    }

    pub fn id(&self) -> Option<&RequestId> {
        match self {
            Self::Request { id, .. } | Self::Response { id, .. } => Some(id),
            Self::Notification { .. } | Self::ProtocolError { .. } => None,
        }
    }
}
