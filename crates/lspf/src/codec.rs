//! The single JSON-RPC params/result codec (ADR 0017's "exactly one decode
//! per dispatched method and exactly one result encode per request" rule).
//!
//! The protocol engine uses these functions at its wire boundary. Normalized
//! user calls cross the Service stack as `serde_json::Value`, and typed erased
//! handlers convert between that decoded representation and native Rust values
//! without introducing another byte or string boundary.

use bytes::Bytes;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::LspError;

/// Decode a JSON-RPC params payload into `P`, mapping any failure to
/// [`LspError::InvalidParams`] (-32602) so a malformed request never reaches a
/// handler. An empty payload is treated as an empty object, so parameterless
/// methods decode cleanly.
pub(crate) fn decode_params<P: DeserializeOwned>(params: &Bytes) -> Result<P, LspError> {
    let bytes: &[u8] = if params.is_empty() { b"{}" } else { params };
    serde_json::from_slice(bytes).map_err(LspError::invalid_params)
}

/// Encode a success value to its JSON body bytes exactly once, mapping a
/// serialization failure to an internal error.
pub(crate) fn encode_body<R: Serialize>(value: &R) -> Result<Bytes, LspError> {
    serde_json::to_vec(value)
        .map(Bytes::from)
        .map_err(|e| LspError::internal(format!("serialization failed: {e}")))
}
