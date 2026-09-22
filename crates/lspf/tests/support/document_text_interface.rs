//! Shared downstream signature checks, compiled on native and real WASM.
use lspf::types::{Position, Range};
use lspf::{Document, PositionEncoding};
use std::borrow::Cow;

#[allow(dead_code)]
pub fn document_text_helpers(document: &Document, encoding: PositionEncoding) {
    let _: Option<Cow<'_, str>> = document.line(0);
    let _: Option<Cow<'_, str>> = document.text_in_range(
        encoding,
        Range::new(Position::new(0, 0), Position::new(0, 0)),
    );
    let _: Option<(Cow<'_, str>, Range)> =
        document.word_at_position(encoding, Position::new(0, 0), |ch| {
            ch.is_alphanumeric() || ch == '_'
        });
    // The pre-existing signatures remain usable alongside partial reads.
    let _: String = document.text();
    let _: Option<usize> = document.position_to_offset(encoding, Position::new(0, 0));
    let _: Option<Position> = document.offset_to_position(encoding, 0);
}
