//! Internal fuzz drivers for protocol and document boundaries.
//!
//! This module is intentionally hidden from generated documentation and is
//! available only through the repository's `fuzzing` feature.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use bytes::{Bytes, BytesMut};
use gen_lsp_types::{Position, Range, Uri};
use tokio_util::codec::{Decoder, Encoder};

use crate::documents::{Document, PositionEncoding};
use crate::transport::envelope;
use crate::transport::framing::ContentLengthCodec;
use crate::uri_key::{UriKey, percent_decode};

const LARGE_INPUT_LIMIT: usize = 65_536;
const URI_INPUT_LIMIT: usize = 4_096;
const LIFECYCLE_INPUT_LIMIT: usize = 16_384;

/// Exercise JSON-RPC envelope parsing and canonical serialization.
pub fn envelope(data: &[u8]) {
    if data.len() > LARGE_INPUT_LIMIT {
        return;
    }

    let parsed = envelope::parse(Bytes::copy_from_slice(data));
    let serialized = envelope::serialize(&parsed).expect("parsed envelope must serialize");
    // Assert well-formedness, not `serde_json::Value` representability. `Value`
    // stores every number as an `f64`, so asserting on it rejects large but
    // perfectly legal JSON such as `1e999`, which the framework forwards
    // verbatim and any peer is free to accept. Escapes that genuinely cannot
    // reach the wire, unpaired UTF-16 surrogates, are rejected by
    // `envelope::parse` instead and covered by its own tests.
    serde_json::from_slice::<&serde_json::value::RawValue>(&serialized)
        .expect("serialized envelope must be well-formed JSON");

    if !matches!(parsed, crate::RawMessage::ProtocolError { .. }) {
        let reparsed = envelope::parse(Bytes::from(serialized.clone()));
        let reserialized =
            envelope::serialize(&reparsed).expect("reparsed envelope must serialize");
        assert_eq!(
            serialized, reserialized,
            "valid envelope serialization is unstable"
        );
    }
}

/// Exercise incremental Content-Length decoding and body re-encoding.
pub fn content_length(data: &[u8]) {
    if data.len() > LARGE_INPUT_LIMIT {
        return;
    }

    let expanded = expand_visible_crlf(data);
    let mut codec = ContentLengthCodec::new(LARGE_INPUT_LIMIT);
    let mut input = BytesMut::new();

    for byte in expanded {
        input.extend_from_slice(&[byte]);
        loop {
            let decoded = match codec.decode(&mut input) {
                Ok(Some(body)) => body,
                Ok(None) | Err(_) => break,
            };
            let mut encoded = BytesMut::new();
            codec
                .encode(decoded.clone(), &mut encoded)
                .expect("a decoded bounded body must re-encode");

            let mut verifier = ContentLengthCodec::new(LARGE_INPUT_LIMIT);
            assert_eq!(
                verifier
                    .decode(&mut encoded)
                    .expect("encoded frame decodes"),
                Some(decoded)
            );
            assert!(encoded.is_empty());
        }
    }
}

/// Exercise URI parsing, normalization, percent decoding, and hash identity.
pub fn uri_identity(data: &[u8]) {
    if data.len() > URI_INPUT_LIMIT {
        return;
    }

    let (left, right) = if let Some(split) = data.iter().position(|byte| *byte == 0) {
        (&data[..split], &data[split + 1..])
    } else {
        (data, data)
    };
    let left_text = String::from_utf8_lossy(left);
    let right_text = String::from_utf8_lossy(right);
    let _ = percent_decode(&left_text);
    let _ = percent_decode(&right_text);

    let (Ok(left_uri), Ok(right_uri)) = (
        Uri::from_str(left_text.trim()),
        Uri::from_str(right_text.trim()),
    ) else {
        return;
    };
    let left_key = UriKey::new(&left_uri);
    let right_key = UriKey::new(&right_uri);
    if left_key == right_key {
        assert_eq!(
            hash(&left_key),
            hash(&right_key),
            "equal URI keys hash differently"
        );
    }
}

/// Exercise UTF-8 and UTF-16 position conversion over arbitrary Unicode text.
pub fn position_conversion(data: &[u8]) {
    if data.len() > LARGE_INPUT_LIMIT {
        return;
    }

    let text = String::from_utf8_lossy(data).into_owned();
    let uri = Uri::from_str("file:///fuzz.txt").expect("static URI parses");
    let document = Document::provider_snapshot(uri, text.clone());

    for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        for offset in text
            .char_indices()
            .map(|(offset, _)| offset)
            .chain(std::iter::once(text.len()))
        {
            let position = document
                .offset_to_position(encoding, offset)
                .expect("in-range character boundary has a position");
            if let Some(roundtrip) = document.position_to_offset(encoding, position) {
                assert_eq!(offset, roundtrip, "position conversion did not round-trip");
            }
        }

        if data.len() >= 8 {
            let position = Position::new(read_u32(&data[..4]), read_u32(&data[4..8]));
            let _ = document.position_to_offset(encoding, position);
        }
    }
}

/// Exercise incremental edits, including invalid and reversed ranges.
pub fn incremental_edits(data: &[u8]) {
    if data.len() > LARGE_INPUT_LIMIT {
        return;
    }

    let controls = data.get(..9).unwrap_or(data);
    let payload = data.get(controls.len()..).unwrap_or_default();
    let split = payload
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(payload.len());
    let initial = String::from_utf8_lossy(&payload[..split]).into_owned();
    let replacement =
        String::from_utf8_lossy(payload.get(split + 1..).unwrap_or_default()).into_owned();
    let uri = Uri::from_str("file:///fuzz.txt").expect("static URI parses");
    let mut document = Document::provider_snapshot(uri, initial.clone());
    let encoding = if controls.first().copied().unwrap_or_default() & 1 == 0 {
        PositionEncoding::Utf8
    } else {
        PositionEncoding::Utf16
    };
    let change = if controls.first().copied().unwrap_or_default() % 5 == 0 {
        gen_lsp_types::TextDocumentContentChangeWholeDocument::new(replacement).into()
    } else {
        gen_lsp_types::TextDocumentContentChangePartial::new(
            Range::new(
                position_from(controls.get(1..5).unwrap_or_default()),
                position_from(controls.get(5..9).unwrap_or_default()),
            ),
            None,
            replacement,
        )
        .into()
    };

    let before = document.text();
    let result = document.apply_change(encoding, change, LARGE_INPUT_LIMIT);
    if result.is_err() {
        assert_eq!(
            document.text(),
            before,
            "a rejected edit mutated the document"
        );
    } else {
        assert!(document.text().len() <= LARGE_INPUT_LIMIT);
    }
}

/// Exercise arbitrary operations against the production Client lifecycle.
pub fn lifecycle_sequences(data: &[u8]) {
    if data.len() <= LIFECYCLE_INPUT_LIMIT {
        crate::client_endpoint::fuzz_lifecycle_sequence(data);
    }
}

fn expand_visible_crlf(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len());
    let mut index = 0;
    while index < data.len() {
        if data[index..].starts_with(br"\r") {
            output.push(b'\r');
            index += 2;
        } else if data[index..].starts_with(br"\n") {
            output.push(b'\n');
            index += 2;
        } else {
            output.push(data[index]);
            index += 1;
        }
    }
    output
}

fn position_from(bytes: &[u8]) -> Position {
    let mut padded = [0; 4];
    padded[..bytes.len().min(4)].copy_from_slice(&bytes[..bytes.len().min(4)]);
    Position::new(
        u16::from_le_bytes([padded[0], padded[1]]) as u32,
        u16::from_le_bytes([padded[2], padded[3]]) as u32,
    )
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("caller supplies four bytes"))
}

fn hash(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}
