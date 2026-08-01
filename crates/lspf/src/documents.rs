use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use lsp_types::{
    InitializeParams, Position, PositionEncodingKind, TextDocumentContentChangeEvent,
    TextDocumentItem, Uri,
};
use ropey::Rope;

/// Negotiated meaning of `Position.character` (ADR 0016).
///
/// LSP defaults to UTF-16; lspf prefers UTF-8 when the client offers it.
/// The store's current value governs every `position ↔ offset` conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PositionEncoding {
    /// `Position.character` is a UTF-8 byte offset within the line.
    Utf8,
    /// `Position.character` is a UTF-16 code-unit offset within the line.
    ///
    /// LSP-mandatory default until UTF-8 negotiation (issue #10) overwrites it.
    #[default]
    Utf16,
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

    /// Apply one content change to this document's text, interpreting `range`
    /// under `encoding`. An absent range replaces the whole document.
    ///
    /// Leaves the text untouched when the change is rejected, so a caller
    /// applying a batch can abandon a working copy without having corrupted
    /// anything.
    fn apply_change(
        &mut self,
        encoding: PositionEncoding,
        change: TextDocumentContentChangeEvent,
    ) -> std::result::Result<(), crate::LspError> {
        let Some(range) = change.range else {
            self.text = Rope::from_str(&change.text);
            return Ok(());
        };
        let start_offset = self
            .position_to_offset(encoding, range.start)
            .ok_or_else(|| crate::LspError::invalid_request("invalid start position"))?;
        let end_offset = self
            .position_to_offset(encoding, range.end)
            .ok_or_else(|| crate::LspError::invalid_request("invalid end position"))?;
        // A reversed range (end before start) would panic `Rope::remove`
        // while the write lock is held, poisoning the store for every
        // later access. Reject it as an invalid request instead.
        if start_offset > end_offset {
            return Err(crate::LspError::invalid_request(
                "range end precedes range start",
            ));
        }
        let start_char = self.text.byte_to_char(start_offset);
        let end_char = self.text.byte_to_char(end_offset);
        self.text.remove(start_char..end_char);
        self.text.insert(start_char, &change.text);
        Ok(())
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
        self.apply_changes(uri, version, [change])
            .map_err(Into::into)
    }

    /// Apply one `didChange` notification's content changes in order,
    /// advancing the document to `version` (ADR 0018).
    ///
    /// The batch is all-or-nothing: the changes compose against a working copy,
    /// and the document — text and version alike — is replaced only once every
    /// change has applied. A rejected change therefore leaves the document
    /// exactly as the last accepted notification left it, never at a
    /// half-applied revision no reader asked for.
    pub(crate) fn apply_changes(
        &self,
        uri: &Uri,
        version: i32,
        changes: impl IntoIterator<Item = TextDocumentContentChangeEvent>,
    ) -> std::result::Result<(), crate::LspError> {
        let mut inner = self.inner.write().unwrap();
        let encoding = inner.encoding;
        let doc = inner
            .by_uri
            .get_mut(uri)
            .ok_or_else(|| crate::LspError::invalid_request("document not found"))?;

        // `Rope` clones share their nodes, so the working copy costs little
        // more than the edits it actually makes.
        let mut updated = doc.clone();
        for change in changes {
            updated.apply_change(encoding, change)?;
        }
        updated.version = version;
        *doc = updated;
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

    /// A read-only view of these documents, for handing to user code.
    pub(crate) fn view(&self) -> DocumentsView {
        DocumentsView {
            documents: self.clone(),
        }
    }

    /// Negotiate the position encoding from `InitializeParams`, store it, and
    /// return the LSP kind to advertise in `InitializeResult` (ADR 0016).
    ///
    /// lspf intersects the client's offered `general.positionEncodings` with
    /// its own preference order (`utf-8` then `utf-16`). A client that offers
    /// nothing, nothing supported, or omits the field defaults to UTF-16.
    pub(crate) fn negotiate_position_encoding(
        &self,
        params: &InitializeParams,
    ) -> PositionEncodingKind {
        let offered = params
            .capabilities
            .general
            .as_ref()
            .and_then(|g| g.position_encodings.as_deref());

        let preferred = [PositionEncodingKind::UTF8, PositionEncodingKind::UTF16];
        let chosen = offered
            .and_then(|encodings| {
                preferred
                    .iter()
                    .find(|kind| encodings.contains(kind))
                    .cloned()
            })
            .unwrap_or(PositionEncodingKind::UTF16);

        self.set_position_encoding(if chosen == PositionEncodingKind::UTF8 {
            PositionEncoding::Utf8
        } else {
            PositionEncoding::Utf16
        });
        chosen
    }
}

/// The read-only view of the connection's [`Documents`] that
/// [`Context::documents`](crate::Context::documents) hands to user code
/// (ADR 0018).
///
/// It carries the retained [`Document`] lookup and the position-conversion
/// behavior, and deliberately nothing else: there is no `open`, `close`, or
/// change operation to call. Documents change only through the protocol
/// engine's built-in `didOpen`, `didChange`, and `didClose` handling, and a
/// registered post-mutation hook observes the result.
///
/// Cheap to clone — every copy reads the documents the connection owns.
#[derive(Debug, Clone)]
pub struct DocumentsView {
    documents: Documents,
}

impl DocumentsView {
    /// Read a snapshot of a document by URI, or `None` if it is not open.
    pub fn get(&self, uri: &Uri) -> Option<Document> {
        self.documents.get(uri)
    }

    /// Convert a position in `uri` to a byte offset using the connection's
    /// negotiated encoding. `None` if the document is not open or the position
    /// is out of range.
    pub fn position_to_offset(&self, uri: &Uri, position: Position) -> Option<usize> {
        self.documents.position_to_offset(uri, position)
    }

    /// Convert a byte offset in `uri` to a position using the connection's
    /// negotiated encoding. `None` if the document is not open or the offset is
    /// out of range.
    pub fn offset_to_position(&self, uri: &Uri, offset: usize) -> Option<Position> {
        self.documents.offset_to_position(uri, offset)
    }

    /// The encoding `Position.character` is measured in for this connection,
    /// negotiated during initialization (ADR 0016).
    pub fn position_encoding(&self) -> PositionEncoding {
        self.documents.position_encoding()
    }
}
