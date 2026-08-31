//! Regressions for crashes the `envelope` fuzz target found.
//!
//! Each test drives `lspf::fuzzing::envelope` with the exact reproducer, so a
//! regression fails here in seconds instead of only inside a scheduled fuzz run.

#![cfg(feature = "fuzzing")]

/// Gate D run 33346221383 (`envelope`): a request whose string payload carries
/// an unpaired UTF-16 high surrogate escape. Canonical serialization emitted
/// that escape unpaired, and re-parsing the output failed with "lone leading
/// surrogate in hex escape".
///
/// The reproducer is a fixture rather than a literal because it mixes bare
/// carriage returns with backslash-u escape text, which is unreadable and easy
/// to corrupt when transcribed by hand.
#[test]
fn unpaired_high_surrogate_escape_serializes_to_valid_json() {
    lspf::fuzzing::envelope(include_bytes!("fixtures/envelope_unpaired_surrogate.json"));
}

/// Gate D run 33357471302 (`envelope`): a JSON number whose exponent overflows
/// the `f64` that `serde_json::Value` stores every number in, reported as
/// "number out of range".
///
/// The two reproducers here are green for opposite reasons, which is the point
/// of keeping both. The surrogate above is rejected by `envelope::parse`,
/// because it denotes a character with no UTF-8 encoding and so can never reach
/// a peer. This number is *accepted* and forwarded unchanged, because it is
/// legal JSON that a peer is free to read; only `Value` cannot hold it, and the
/// assertion that used to conflate the two was relaxed to well-formedness.
#[test]
fn out_of_range_number_is_forwarded_as_well_formed_json() {
    lspf::fuzzing::envelope(include_bytes!("fixtures/envelope_number_out_of_range.json"));
}
