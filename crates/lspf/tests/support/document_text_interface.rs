//! Shared downstream signature checks, compiled on native and real WASM.
use lspf::types::{Position, Range};
use lspf::{Document, PositionEncoding};
use std::borrow::Cow;

#[allow(dead_code)]
pub fn document_text_helpers(document: &Document, encoding: PositionEncoding) {
    let _: usize = document.line_count();
    let range: Option<Range> = document.line_range(encoding, 0);
    let _: Option<Cow<'_, str>> = document.text(encoding, None);
    if let Some(range) = range {
        let _: Option<Cow<'_, str>> = document.text(encoding, Some(range));
    }
    let _: Option<(Cow<'_, str>, Range)> =
        document.word_at_position(encoding, Position::new(0, 0), |ch| {
            ch.is_alphanumeric() || ch == '_'
        });
    let _: Option<usize> = document.position_to_offset(encoding, Position::new(0, 0));
    let _: Option<Position> = document.offset_to_position(encoding, 0);
}
