use std::borrow::Cow;
use std::io::Write;

use bytes::Bytes;
use lsp_types::NumberOrString;
use serde::Deserialize;
use serde_json::value::RawValue;

use super::TransportError;
use crate::raw::{JsonRpcError, RawMessage};

#[derive(Deserialize)]
struct InEnvelope<'a> {
    jsonrpc: &'a str,
    #[serde(default)]
    id: Option<NumberOrString>,
    #[serde(default, borrow)]
    method: Option<&'a str>,
    #[serde(default, borrow)]
    params: Option<&'a RawValue>,
    #[serde(default, borrow)]
    result: Option<&'a RawValue>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

pub fn parse(body: Bytes) -> Result<RawMessage, TransportError> {
    let env: InEnvelope = serde_json::from_slice(&body)?;
    if env.jsonrpc != "2.0" {
        return Err(TransportError::Malformed(format!(
            "unsupported jsonrpc version: {}",
            env.jsonrpc
        )));
    }

    let params_bytes = env
        .params
        .map(|rv| Bytes::copy_from_slice(rv.get().as_bytes()))
        .unwrap_or_default();

    match (env.id, env.method, env.result, env.error) {
        (Some(id), Some(method), _, _) => Ok(RawMessage::Request {
            id,
            method: Cow::Owned(method.to_string()),
            params: params_bytes,
        }),
        (None, Some(method), _, _) => Ok(RawMessage::Notification {
            method: Cow::Owned(method.to_string()),
            params: params_bytes,
        }),
        (Some(id), None, Some(result), None) => Ok(RawMessage::Response {
            id,
            result: Ok(Bytes::copy_from_slice(result.get().as_bytes())),
        }),
        (Some(id), None, None, Some(error)) => Ok(RawMessage::Response {
            id,
            result: Err(error),
        }),
        _ => Err(TransportError::Malformed(
            "envelope is neither request, notification, nor response".into(),
        )),
    }
}

pub fn serialize(msg: &RawMessage) -> Result<Vec<u8>, TransportError> {
    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(br#"{"jsonrpc":"2.0""#);
    match msg {
        RawMessage::Request { id, method, params } => {
            out.extend_from_slice(br#","id":"#);
            serde_json::to_writer(&mut out, id)?;
            out.extend_from_slice(br#","method":"#);
            serde_json::to_writer(&mut out, method.as_ref())?;
            write_params(&mut out, params);
        }
        RawMessage::Notification { method, params } => {
            out.extend_from_slice(br#","method":"#);
            serde_json::to_writer(&mut out, method.as_ref())?;
            write_params(&mut out, params);
        }
        RawMessage::Response { id, result } => {
            out.extend_from_slice(br#","id":"#);
            serde_json::to_writer(&mut out, id)?;
            match result {
                Ok(result_bytes) => {
                    out.extend_from_slice(br#","result":"#);
                    if result_bytes.is_empty() {
                        out.extend_from_slice(b"null");
                    } else {
                        out.extend_from_slice(result_bytes);
                    }
                }
                Err(err) => {
                    out.extend_from_slice(br#","error":"#);
                    serde_json::to_writer(&mut out, err)?;
                }
            }
        }
    }
    out.push(b'}');
    Ok(out)
}

fn write_params(out: &mut Vec<u8>, params: &Bytes) {
    if params.is_empty() {
        return;
    }
    out.extend_from_slice(br#","params":"#);
    let _ = out.write_all(params);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_request() {
        let body = Bytes::from_static(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"foo":42}}"#,
        );
        let msg = parse(body).unwrap();
        match &msg {
            RawMessage::Request { id, method, params } => {
                assert_eq!(method, "initialize");
                assert!(matches!(id, NumberOrString::Number(1)));
                assert_eq!(&params[..], br#"{"foo":42}"#);
            }
            _ => panic!("expected request"),
        }
        let out = serialize(&msg).unwrap();
        let out_str = std::str::from_utf8(&out).unwrap();
        assert!(out_str.starts_with(r#"{"jsonrpc":"2.0""#));
        assert!(
            out_str.contains(r#""method":"initialize""#),
            "got: {out_str}"
        );
        assert!(out_str.contains(r#""params":{"foo":42}"#), "got: {out_str}");
    }

    #[test]
    fn roundtrips_notification() {
        let body = Bytes::from_static(br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#);
        let msg = parse(body).unwrap();
        assert!(matches!(&msg, RawMessage::Notification { .. }));
    }

    #[test]
    fn serializes_null_result() {
        let msg = RawMessage::Response {
            id: NumberOrString::Number(7),
            result: Ok(Bytes::new()),
        };
        let out = serialize(&msg).unwrap();
        let expected = br#"{"jsonrpc":"2.0","id":7,"result":null}"#;
        assert_eq!(&out, expected);
    }
}
