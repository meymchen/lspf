use std::io::Write;

use bytes::Bytes;
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use std::borrow::Cow;

use super::TransportError;
use crate::raw::{JsonRpcError, RawMessage, RequestId};

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
    // JSON-RPC 2.0 requires a present `params` to be structured, but LSP
    // clients routinely send `"params": null` for the parameterless methods
    // (`shutdown`, `exit`). An explicit null carries no arguments, so accept it
    // as the absent member it stands for rather than failing the envelope.
    let params_raw = env.params.0.filter(|value| !is_json_null(value));
    if !params_are_structured(params_raw) {
        return invalid_request();
    }
    // `params` and `result` are forwarded verbatim, so anything accepted here
    // must still be JSON a strict peer can parse.
    if [params_raw, env.result.0]
        .into_iter()
        .flatten()
        .any(|value| has_unpaired_surrogate_escape(value.get()))
    {
        return protocol_error(-32700, "Parse error");
    }

    let has_params = params_raw.is_some();
    let has_id = env.id.0.is_some();
    let params = params_raw
        .map(|value| Bytes::copy_from_slice(value.get().as_bytes()))
        .unwrap_or_default();

    let method = env
        .method
        .0
        .and_then(|value| serde_json::from_str::<String>(value.get()).ok());
    let id = env
        .id
        .0
        .and_then(|value| serde_json::from_str::<RequestId>(value.get()).ok());

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

/// Report whether raw JSON text contains a UTF-16 surrogate escape that is not
/// part of a valid pair.
///
/// `params` and `result` are forwarded byte for byte, so an escape that no
/// strict parser will accept would otherwise survive the round trip and be
/// re-emitted to a peer. `RawValue` only guarantees that `\u` is followed by
/// four hex digits; surrogate pairing is checked when a `String` is
/// materialized, which never happens for these two fields.
///
/// This is one pass over bytes the caller is about to copy anyway, so it does
/// not change the cost class of accepting a message.
fn has_unpaired_surrogate_escape(text: &str) -> bool {
    fn code_unit(bytes: &[u8]) -> Option<u32> {
        let digits = bytes.get(..4)?;
        u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
    }

    const HIGH: std::ops::Range<u32> = 0xD800..0xDC00;
    const LOW: std::ops::Range<u32> = 0xDC00..0xE000;

    let bytes = text.as_bytes();
    let mut index = 0;
    let mut in_string = false;
    while index < bytes.len() {
        if !in_string {
            in_string = bytes[index] == b'"';
            index += 1;
            continue;
        }
        match bytes[index] {
            b'"' => {
                in_string = false;
                index += 1;
            }
            b'\\' if bytes.get(index + 1) == Some(&b'u') => {
                let Some(unit) = code_unit(&bytes[index + 2..]) else {
                    return true;
                };
                index += 6;
                if HIGH.contains(&unit) {
                    // A high surrogate is well formed only when the very next
                    // escape is a low surrogate.
                    if bytes.get(index) != Some(&b'\\') || bytes.get(index + 1) != Some(&b'u') {
                        return true;
                    }
                    let Some(low) = code_unit(&bytes[index + 2..]) else {
                        return true;
                    };
                    if !LOW.contains(&low) {
                        return true;
                    }
                    index += 6;
                } else if LOW.contains(&unit) {
                    // A low surrogate reached without a high one before it.
                    return true;
                }
            }
            // Any other two-character escape, including `\\` and `\"`.
            b'\\' => index += 2,
            _ => index += 1,
        }
    }
    false
}

/// Report whether a raw member is the JSON literal `null`.
fn is_json_null(value: &RawValue) -> bool {
    value.get().trim_matches(|c: char| c.is_ascii_whitespace()) == "null"
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
                assert!(matches!(id, RequestId::Number(1)));
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

    // Adapted from clangd's JSON transport coverage: JSON-RPC permits request
    // ids to be strings, and the exact value must survive the wire round trip.
    #[test]
    fn roundtrips_string_request_id() {
        let body = Bytes::from_static(
            br#"{"jsonrpc":"2.0","id":"editor-request","method":"initialize","params":{}}"#,
        );
        let msg = parse(body);

        assert!(matches!(
            msg.id(),
            Some(RequestId::String(id)) if id == "editor-request"
        ));
        assert_eq!(
            serialize(&msg).unwrap(),
            br#"{"jsonrpc":"2.0","id":"editor-request","method":"initialize","params":{}}"#
        );
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
            id: RequestId::Number(7),
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

    // LSP clients send `"params": null` for the parameterless methods. The
    // member carries no arguments, so it must decode exactly like an omitted
    // one instead of failing the envelope.
    #[test]
    fn explicit_null_params_decode_as_absent_params() {
        for body in [
            br#"{"jsonrpc":"2.0","id":1,"method":"shutdown","params":null}"#.as_slice(),
            br#"{"jsonrpc":"2.0","method":"exit","params": null }"#.as_slice(),
        ] {
            match parse(Bytes::copy_from_slice(body)) {
                RawMessage::Request { params, .. } | RawMessage::Notification { params, .. } => {
                    assert!(
                        params.is_empty(),
                        "expected absent params for {}",
                        String::from_utf8_lossy(body)
                    );
                }
                other => panic!("expected a dispatchable message, got {other:?}"),
            }
        }
    }

    // A response is distinguished from a request by carrying no `params`, and
    // an explicit null is no `params`.
    #[test]
    fn explicit_null_params_still_parse_a_response() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":{},"params":null}"#;

        assert!(matches!(
            parse(Bytes::copy_from_slice(body)),
            RawMessage::Response { .. }
        ));
    }

    // `params` and `result` are forwarded byte for byte, so an unpaired
    // surrogate escape would otherwise be re-emitted as JSON a strict peer
    // cannot decode. Found by the `envelope` fuzz target.
    #[test]
    fn rejects_unpaired_surrogate_escapes_in_forwarded_payloads() {
        for body in [
            br#"{"jsonrpc":"2.0","method":"m","params":{"a":"\uDBFE"}}"#.as_slice(),
            br#"{"jsonrpc":"2.0","method":"m","params":["\uDC00"]}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":1,"method":"m","params":["\uD800\uD800"]}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":1,"result":"\uDBFE"}"#.as_slice(),
        ] {
            match parse(Bytes::copy_from_slice(body)) {
                RawMessage::ProtocolError { error } => assert_eq!(
                    error.code,
                    -32700,
                    "expected a parse error for {}",
                    String::from_utf8_lossy(body)
                ),
                other => panic!("expected parse error, got {other:?}"),
            }
        }
    }

    #[test]
    fn accepts_well_formed_escapes_that_only_look_like_surrogates() {
        for body in [
            // A valid pair encoding U+1F600.
            b"{\"jsonrpc\":\"2.0\",\"method\":\"m\",\"params\":[\"\\uD83D\\uDE00\"]}".as_slice(),
            // An escaped backslash, so `uDBFE` is literal text, not an escape.
            b"{\"jsonrpc\":\"2.0\",\"method\":\"m\",\"params\":[\"\\\\uDBFE\"]}".as_slice(),
            // An escaped quote must not end the string scan early.
            b"{\"jsonrpc\":\"2.0\",\"method\":\"m\",\"params\":[\"\\\"\\uD83D\\uDE00\"]}"
                .as_slice(),
            // Surrogate-looking text outside any string.
            br#"{"jsonrpc":"2.0","method":"m","params":{"uDBFE":1}}"#.as_slice(),
        ] {
            let msg = parse(Bytes::copy_from_slice(body));
            assert!(
                !matches!(msg, RawMessage::ProtocolError { .. }),
                "wrongly rejected {}",
                String::from_utf8_lossy(body)
            );
            // Whatever is accepted must re-serialize into decodable JSON.
            let out = serialize(&msg).unwrap();
            serde_json::from_slice::<serde_json::Value>(&out)
                .expect("accepted envelope re-serializes as valid JSON");
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
