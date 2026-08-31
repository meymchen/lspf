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
