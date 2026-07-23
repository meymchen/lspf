use std::borrow::Cow;
use std::io::Write;

use bytes::Bytes;
use lsp_types::NumberOrString;
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

use super::TransportError;
use crate::raw::{JsonRpcError, RawMessage};

#[derive(Deserialize)]
struct InEnvelope<'a> {
    #[serde(default, borrow)]
    jsonrpc: OptionalRaw<'a>,
    #[serde(default, borrow)]
    id: OptionalRaw<'a>,
    #[serde(default, borrow)]
    method: OptionalRaw<'a>,
    #[serde(default, borrow)]
    params: OptionalRaw<'a>,
    #[serde(default, borrow)]
    result: OptionalRaw<'a>,
    #[serde(default, borrow)]
    error: OptionalRaw<'a>,
}

#[derive(Default)]
struct OptionalRaw<'a>(Option<&'a RawValue>);

impl<'de: 'a, 'a> Deserialize<'de> for OptionalRaw<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        <&'a RawValue>::deserialize(deserializer).map(|value| Self(Some(value)))
    }
}

pub fn parse(body: Bytes) -> RawMessage {
    let env: InEnvelope<'_> = match serde_json::from_slice(&body) {
        Ok(env) => env,
        Err(_) if serde_json::from_slice::<&RawValue>(&body).is_ok() => return invalid_request(),
        Err(_) => return protocol_error(-32700, "Parse error"),
    };

    let Some(jsonrpc) = env.jsonrpc.0 else {
        return invalid_request();
    };
    if serde_json::from_str::<&str>(jsonrpc.get()).ok() != Some("2.0") {
        return invalid_request();
    }
    if !params_are_structured(env.params.0) {
        return invalid_request();
    }

    let has_params = env.params.0.is_some();
    let has_id = env.id.0.is_some();
    let params = env
        .params
        .0
        .map(|value| Bytes::copy_from_slice(value.get().as_bytes()))
        .unwrap_or_default();

    let method = env
        .method
        .0
        .and_then(|value| serde_json::from_str::<String>(value.get()).ok());
    let id = env
        .id
        .0
        .and_then(|value| serde_json::from_str::<NumberOrString>(value.get()).ok());

    match (method, has_id, id, env.result.0, env.error.0) {
        (Some(method), true, Some(id), None, None) => RawMessage::Request {
            id,
            method: Cow::Owned(method),
            params,
        },
        (Some(method), false, None, None, None) => RawMessage::Notification {
            method: Cow::Owned(method),
            params,
        },
        (None, true, Some(id), Some(result), None) if !has_params => RawMessage::Response {
            id,
            result: Ok(Bytes::copy_from_slice(result.get().as_bytes())),
        },
        (None, true, Some(id), None, Some(error)) if !has_params => {
            match serde_json::from_str::<JsonRpcError>(error.get()) {
                Ok(error) => RawMessage::Response {
                    id,
                    result: Err(error),
                },
                Err(_) => invalid_request(),
            }
        }
        _ => invalid_request(),
    }
}

fn invalid_request() -> RawMessage {
    protocol_error(-32600, "Invalid Request")
}

fn params_are_structured(params: Option<&RawValue>) -> bool {
    params.is_none_or(|params| {
        matches!(
            params
                .get()
                .bytes()
                .find(|byte| !byte.is_ascii_whitespace()),
            Some(b'{' | b'[')
        )
    })
}

fn protocol_error(code: i32, message: &str) -> RawMessage {
    RawMessage::ProtocolError {
        error: JsonRpcError {
            code,
            message: message.into(),
            data: None,
        },
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
        RawMessage::ProtocolError { error } => {
            out.extend_from_slice(b",\"id\":null,\"error\":");
            serde_json::to_writer(&mut out, error)?;
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
        let msg = parse(body);
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
        let msg = parse(body);
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

    #[test]
    fn malformed_json_becomes_a_null_id_parse_error() {
        let msg = parse(Bytes::from_static(
            br#"{"jsonrpc":"2.0","method":"initialize""#,
        ));

        match msg {
            RawMessage::ProtocolError { error } => {
                assert_eq!(error.code, -32700);
                assert_eq!(error.message, "Parse error");
                assert_eq!(error.data, None);
            }
            other => panic!("expected parse error response, got {other:?}"),
        }
    }

    #[test]
    fn non_object_json_becomes_a_null_id_invalid_request() {
        let msg = parse(Bytes::from_static(br#"null"#));

        match msg {
            RawMessage::ProtocolError { error } => {
                assert_eq!(error.code, -32600);
                assert_eq!(error.message, "Invalid Request");
            }
            other => panic!("expected invalid request response, got {other:?}"),
        }
    }

    #[test]
    fn invalid_envelopes_become_null_id_invalid_requests() {
        for body in [
            br#"{"jsonrpc":"1.0","method":"initialize"}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":null,"method":"initialize"}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":true,"method":"initialize"}"#.as_slice(),
            br#"{"jsonrpc":"2.0","method":"initialize","params":1}"#.as_slice(),
        ] {
            match parse(Bytes::copy_from_slice(body)) {
                RawMessage::ProtocolError { error } => assert_eq!(error.code, -32600),
                other => panic!("expected invalid request response, got {other:?}"),
            }
        }
    }

    #[test]
    fn serializes_protocol_error_with_a_null_id() {
        let msg = protocol_error(-32600, "Invalid Request");

        let out = serialize(&msg).unwrap();

        assert_eq!(
            out,
            br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid Request"}}"#,
        );
    }
}
