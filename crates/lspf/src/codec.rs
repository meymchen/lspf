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
use serde_json::Value;

use crate::error::LspError;

/// Decode a JSON-RPC params payload into `P`, mapping any failure to
/// [`LspError::InvalidParams`] (-32602) so a malformed request never reaches a
/// handler. An empty payload is treated as an empty object, so parameterless
/// methods decode cleanly.
pub(crate) fn decode_params<P: DeserializeOwned>(params: &Bytes) -> Result<P, LspError> {
    let bytes: &[u8] = if params.is_empty() { b"{}" } else { params };
    serde_json::from_slice(bytes).map_err(LspError::invalid_params)
}

/// Decode the transport's parameter bytes into the method-erased value that
/// crosses the Service stack.
pub(crate) fn decode_value(params: &Bytes) -> Result<Value, LspError> {
    decode_params(params)
}

/// Read one optional token field from already-decoded request parameters.
///
/// Both endpoint directions use this lookup so work-done and partial-result
/// token handling agree on missing and explicit-null fields.
pub(crate) fn request_token<T: DeserializeOwned>(
    params: &Value,
    field: &str,
) -> Result<Option<T>, serde_json::Error> {
    let Some(token) = params.get(field) else {
        return Ok(None);
    };
    if token.is_null() {
        return Ok(None);
    }
    serde_json::from_value(token.clone()).map(Some)
}

/// Convert one typed handler result into the decoded, method-erased value that
/// unwinds through the Service stack.
pub(crate) fn erase_value<R: Serialize>(value: R) -> Result<Value, LspError> {
    serde_json::to_value(value)
        .map_err(|error| LspError::internal(format!("serialization failed: {error}")))
}

/// Encode typed JSON-RPC parameters, omitting a serde unit/null payload.
///
/// JSON-RPC permits parameters to be absent, an object, or an array, but not
/// `null`. Rust request markers conventionally use `()` for parameterless
/// methods, so their serialized `null` becomes an absent `params` member.
pub(crate) fn encode_params<P: Serialize>(params: &P) -> Result<Bytes, serde_json::Error> {
    let encoded = serde_json::to_vec(params)?;
    if encoded == b"null" {
        Ok(Bytes::new())
    } else {
        Ok(Bytes::from(encoded))
    }
}

/// Encode a success value to its JSON body bytes exactly once, mapping a
/// serialization failure to an internal error.
pub(crate) fn encode_body<R: Serialize>(value: &R) -> Result<Bytes, LspError> {
    serde_json::to_vec(value)
        .map(Bytes::from)
        .map_err(|e| LspError::internal(format!("serialization failed: {e}")))
}
