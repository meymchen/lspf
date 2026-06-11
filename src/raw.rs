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
            Self::Response { .. } => None,
        }
    }

    pub fn id(&self) -> Option<&RequestId> {
        match self {
            Self::Request { id, .. } | Self::Response { id, .. } => Some(id),
            Self::Notification { .. } => None,
        }
    }
}
