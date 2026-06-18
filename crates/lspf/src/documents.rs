use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use lsp_types::{Position, TextDocumentContentChangeEvent, TextDocumentItem, Uri};
use ropey::Rope;

/// Negotiated meaning of `Position.character` (ADR 0016).
///
/// LSP defaults to UTF-16; lspf prefers UTF-8 when the client offers it.
/// The store's current value governs every `position ↔ offset` conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    /// `Position.character` is a UTF-8 byte offset within the line.
    Utf8,
    /// `Position.character` is a UTF-16 code-unit offset within the line.
    Utf16,
}

impl Default for PositionEncoding {
    fn default() -> Self {
        // LSP-mandatory default until UTF-8 negotiation (issue #10) overwrites it.
        Self::Utf16
    }
}

/// A single tracked text document (ADR 0005).
///
/// Backed by `ropey::Rope`, but `ropey` never leaks into the public API.
/// The document is immutable from user code; mutations flow through the
/// concurrency-safe [`Documents`] handle.
#[derive(Debug, Clone)]
pub struct Document {
    uri: Uri,
    language_id: String,
    version: i32,
    text: Rope,
}

impl Document {
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    pub fn language_id(&self) -> &str {
        &self.language_id
    }

    pub fn version(&self) -> i32 {
        self.version
    }

    /// Full document text as a `String`.
    pub fn text(&self) -> String {
        self.text.to_string()
    }

    /// Convert an LSP `Position` to a byte offset into the rope, using the
    /// supplied encoding. Returns `None` if the position is out of range.
    pub fn position_to_offset(
        &self,
        encoding: PositionEncoding,
        position: Position,
    ) -> Option<usize> {
        let line_idx = position.line as usize;
        if line_idx >= self.text.len_lines() {
            return None;
        }
        let line_start_byte = self.text.line_to_byte(line_idx);
        let line_text: String = self.text.line(line_idx).into();

        match encoding {
            PositionEncoding::Utf8 => {
                let byte_in_line = position.character as usize;
                // `character` is a byte offset, but it must land within the
                // line's content (excluding the trailing line break) and on a
                // UTF-8 char boundary — otherwise the offset would split a
                // codepoint and corrupt a later edit.
                let content_len = line_text.trim_end_matches(['\r', '\n']).len();
                if byte_in_line > content_len || !line_text.is_char_boundary(byte_in_line) {
                    return None;
                }
                Some(line_start_byte + byte_in_line)
            }
            PositionEncoding::Utf16 => {
                let mut utf16_count = 0usize;
                for (byte_idx, ch) in line_text.char_indices() {
                    if utf16_count == position.character as usize {
                        return Some(line_start_byte + byte_idx);
                    }
                    utf16_count += ch.len_utf16();
                }
                if utf16_count == position.character as usize {
                    return Some(line_start_byte + line_text.len());
                }
                None
            }
        }
    }

    /// Convert a byte offset into an LSP `Position`, using the supplied
    /// encoding. Returns `None` if the offset is out of range.
    pub fn offset_to_position(
        &self,
        encoding: PositionEncoding,
        offset: usize,
    ) -> Option<Position> {
        if offset > self.text.len_bytes() {
            return None;
        }
        let line_idx = self.text.byte_to_line(offset);
        let line_start_byte = self.text.line_to_byte(line_idx);
        let line_offset = offset - line_start_byte;
        let line_text: String = self.text.line(line_idx).into();

        match encoding {
            PositionEncoding::Utf8 => Some(Position {
                line: line_idx as u32,
                character: line_offset as u32,
            }),
            PositionEncoding::Utf16 => {
                let mut utf16_count = 0usize;
                for (byte_idx, ch) in line_text.char_indices() {
                    if byte_idx == line_offset {
                        return Some(Position {
                            line: line_idx as u32,
                            character: utf16_count as u32,
                        });
                    }
                    utf16_count += ch.len_utf16();
                }
                Some(Position {
                    line: line_idx as u32,
                    character: utf16_count as u32,
                })
            }
        }
    }
}

#[derive(Debug, Default)]
struct DocumentsInner {
    by_uri: HashMap<Uri, Document>,
    encoding: PositionEncoding,
}

/// Concurrency-safe handle to every tracked [`Document`] (ADR 0003).
///
/// Cheap to clone: all copies share the same `Arc<RwLock<...>>`. Users read
/// freely; mutations happen only through the built-in doc-sync primitives
/// (`open`, `apply_incremental_change`, `close`, `save`).
#[derive(Debug, Clone, Default)]
pub struct Documents {
    inner: Arc<RwLock<DocumentsInner>>,
}

impl Documents {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open or replace a document in the store.
    pub fn open(&self, item: TextDocumentItem) {
        let mut inner = self.inner.write().unwrap();
        inner.by_uri.insert(
            item.uri.clone(),
            Document {
                uri: item.uri,
                language_id: item.language_id,
                version: item.version,
                text: Rope::from_str(&item.text),
            },
        );
    }

    /// Read a snapshot of a document by URI.
    pub fn get(&self, uri: &Uri) -> Option<Document> {
        let inner = self.inner.read().unwrap();
        inner.by_uri.get(uri).cloned()
    }

    /// Remove a document from the store. Returns the removed document, if any.
    pub fn close(&self, uri: &Uri) -> Option<Document> {
        let mut inner = self.inner.write().unwrap();
        inner.by_uri.remove(uri)
    }

    /// Mark a document as saved. Returns `None` if no such document is open.
    ///
    /// The built-in store is in-memory, so this is otherwise a no-op; it
    /// exists as the hook where future persistence logic will attach.
    pub fn save(&self, uri: &Uri) -> Option<()> {
        let inner = self.inner.read().unwrap();
        inner.by_uri.contains_key(uri).then_some(())
    }

    /// Apply an incremental content change to a document, advancing it to
    /// `version`.
    ///
    /// Uses the store's current position encoding to interpret `range`. The
    /// caller passes the version from the `didChange` notification so the
    /// stored [`Document::version`] stays current across edits.
    pub fn apply_incremental_change(
        &self,
        uri: &Uri,
        version: i32,
        change: TextDocumentContentChangeEvent,
    ) -> crate::Result<()> {
        let mut inner = self.inner.write().unwrap();
        let encoding = inner.encoding;
        let doc = inner
            .by_uri
            .get_mut(uri)
            .ok_or_else(|| crate::LspError::invalid_request("document not found"))?;

        if let Some(range) = change.range {
            let start_offset = doc
                .position_to_offset(encoding, range.start)
                .ok_or_else(|| crate::LspError::invalid_request("invalid start position"))?;
            let end_offset = doc
                .position_to_offset(encoding, range.end)
                .ok_or_else(|| crate::LspError::invalid_request("invalid end position"))?;
            // A reversed range (end before start) would panic `Rope::remove`
            // while the write lock is held, poisoning the store for every
            // later access. Reject it as an invalid request instead.
            if start_offset > end_offset {
                return Err(
                    crate::LspError::invalid_request("range end precedes range start").into(),
                );
            }
            let start_char = doc.text.byte_to_char(start_offset);
            let end_char = doc.text.byte_to_char(end_offset);
            doc.text.remove(start_char..end_char);
            doc.text.insert(start_char, &change.text);
        } else {
            doc.text = Rope::from_str(&change.text);
        }
        doc.version = version;
        Ok(())
    }

    /// Convert a position using the store's current encoding.
    pub fn position_to_offset(&self, uri: &Uri, position: Position) -> Option<usize> {
        let inner = self.inner.read().unwrap();
        inner
            .by_uri
            .get(uri)
            .and_then(|doc| doc.position_to_offset(inner.encoding, position))
    }

    /// Convert an offset using the store's current encoding.
    pub fn offset_to_position(&self, uri: &Uri, offset: usize) -> Option<Position> {
        let inner = self.inner.read().unwrap();
        inner
            .by_uri
            .get(uri)
            .and_then(|doc| doc.offset_to_position(inner.encoding, offset))
    }

    /// Current position encoding for every document in the store.
    pub fn position_encoding(&self) -> PositionEncoding {
        self.inner.read().unwrap().encoding
    }

    /// Set the position encoding. Issue #10 calls this from the initialize
    /// handshake; everything else reads it.
    pub fn set_position_encoding(&self, encoding: PositionEncoding) {
        self.inner.write().unwrap().encoding = encoding;
    }
}
