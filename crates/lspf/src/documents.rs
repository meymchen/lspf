use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use lsp_types::{
    InitializeParams, Position, PositionEncodingKind, TextDocumentContentChangeEvent,
    TextDocumentItem, Uri,
};
use ropey::Rope;

use crate::uri_key::UriKey;

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
/// concurrency-safe document store the connection's protocol engine owns.
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
    /// Identity is the normalized [`UriKey`], so equivalent spellings of one
    /// URI address one document; each [`Document`] still carries the original
    /// URI the client opened it with.
    by_uri: HashMap<UriKey, Document>,
    encoding: PositionEncoding,
}

/// Concurrency-safe handle to every tracked [`Document`], owned by the
/// connection's protocol engine (ADR 0003, ADR 0018).
///
/// Cheap to clone: all copies share the same `Arc<RwLock<...>>`. The type is
/// crate-private on purpose — user code never constructs a store, never holds
/// one in its state, and never mutates one. Mutations happen only through the
/// engine's built-in doc-sync handling (`open`, `apply_changes`, `close`), and
/// handlers read through the [`DocumentsView`] the [`Context`] hands them.
///
/// [`Context`]: crate::Context
#[derive(Debug, Clone, Default)]
pub(crate) struct Documents {
    inner: Arc<RwLock<DocumentsInner>>,
}

impl Documents {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Open or replace a document in the store.
    pub(crate) fn open(&self, item: TextDocumentItem) {
        let mut inner = self.inner.write().unwrap();
        inner.by_uri.insert(
            UriKey::new(&item.uri),
            Document {
                uri: item.uri,
                language_id: item.language_id,
                version: item.version,
                text: Rope::from_str(&item.text),
            },
        );
    }

    /// Read a snapshot of a document by URI.
    pub(crate) fn get(&self, uri: &Uri) -> Option<Document> {
        let inner = self.inner.read().unwrap();
        inner.by_uri.get(&UriKey::new(uri)).cloned()
    }

    /// Remove a document from the store. Returns the removed document, if any.
    pub(crate) fn close(&self, uri: &Uri) -> Option<Document> {
        let mut inner = self.inner.write().unwrap();
        inner.by_uri.remove(&UriKey::new(uri))
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
            .get_mut(&UriKey::new(uri))
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
    pub(crate) fn position_to_offset(&self, uri: &Uri, position: Position) -> Option<usize> {
        let inner = self.inner.read().unwrap();
        inner
            .by_uri
            .get(&UriKey::new(uri))
            .and_then(|doc| doc.position_to_offset(inner.encoding, position))
    }

    /// Convert an offset using the store's current encoding.
    pub(crate) fn offset_to_position(&self, uri: &Uri, offset: usize) -> Option<Position> {
        let inner = self.inner.read().unwrap();
        inner
            .by_uri
            .get(&UriKey::new(uri))
            .and_then(|doc| doc.offset_to_position(inner.encoding, offset))
    }

    /// Current position encoding for every document in the store.
    pub(crate) fn position_encoding(&self) -> PositionEncoding {
        self.inner.read().unwrap().encoding
    }

    /// Set the position encoding. Only
    /// [`negotiate_position_encoding`](Self::negotiate_position_encoding), run
    /// once by the initialize transaction, writes it; everything else reads it.
    fn set_position_encoding(&self, encoding: PositionEncoding) {
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

/// The read-only view of the connection's documents that
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
    ///
    /// The lookup resolves through the connection's normalized URI identity
    /// (scheme and host case, percent-encoding, and Windows drive-letter case
    /// are equivalent spellings of one document), while the returned
    /// [`Document`] keeps the original URI the client opened it with.
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use lsp_types::{GeneralClientCapabilities, Range};

    use super::*;

    fn uri(s: &str) -> Uri {
        Uri::from_str(s).unwrap()
    }

    fn text_item(uri: Uri, text: &str) -> TextDocumentItem {
        TextDocumentItem {
            uri,
            language_id: "plaintext".to_string(),
            version: 1,
            text: text.to_string(),
        }
    }

    /// A store holding one already-open document — the state every store test
    /// starts from. Returns the store and the URI the document was opened
    /// under.
    fn opened(name: &str, text: &str) -> (Documents, Uri) {
        let docs = Documents::new();
        let u = uri(name);
        docs.open(text_item(u.clone(), text));
        (docs, u)
    }

    /// One content change, as `didChange` delivers it.
    fn change(range: Option<Range>, text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range,
            range_length: None,
            text: text.to_string(),
        }
    }

    fn range(start: u32, end: u32) -> Range {
        Range {
            start: Position {
                line: 0,
                character: start,
            },
            end: Position {
                line: 0,
                character: end,
            },
        }
    }

    fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    /// `InitializeParams` offering exactly `encodings` under
    /// `general.positionEncodings`.
    fn offering(encodings: Vec<PositionEncodingKind>) -> InitializeParams {
        let mut params = InitializeParams::default();
        params.capabilities.general = Some(GeneralClientCapabilities {
            position_encodings: Some(encodings),
            ..GeneralClientCapabilities::default()
        });
        params
    }

    #[test]
    fn open_document_can_be_read_back() {
        let (docs, u) = opened("file:///tmp/test.txt", "hello world");

        let doc = docs.get(&u).expect("document should exist");
        assert_eq!(doc.uri(), &u);
        assert_eq!(doc.language_id(), "plaintext");
        assert_eq!(doc.version(), 1);
        assert_eq!(doc.text(), "hello world");
    }

    #[test]
    fn close_removes_document() {
        let (docs, u) = opened("file:///close.txt", "x");
        assert!(docs.get(&u).is_some());

        docs.close(&u);
        assert!(docs.get(&u).is_none());
    }

    #[test]
    fn equivalent_uri_spellings_resolve_to_one_document() {
        let docs = Documents::new();
        let original = uri("file:///C%3A/Users/Me/a.rs");
        docs.open(text_item(original.clone(), "fn main() {}"));

        for spelling in [
            "file:///c:/Users/Me/a.rs",
            "file:///C:/Users/Me/a.rs",
            "file:///c%3A/Users/Me/a.rs",
            "file:///c%3a/Users/Me/a.rs",
            "FILE:///C:/Users/Me/a.rs",
        ] {
            let doc = docs
                .get(&uri(spelling))
                .unwrap_or_else(|| panic!("{spelling} resolves to the opened document"));
            assert_eq!(
                doc.uri(),
                &original,
                "the public value keeps the URI the client opened with"
            );
        }
    }

    #[test]
    fn path_case_still_distinguishes_documents() {
        let docs = Documents::new();
        docs.open(text_item(uri("file:///home/Foo.rs"), "a"));
        assert!(
            docs.get(&uri("file:///home/foo.rs")).is_none(),
            "ordinary path case is not normalized"
        );
    }

    #[test]
    fn change_and_close_resolve_through_the_same_normalized_key() {
        let docs = Documents::new();
        docs.open(text_item(uri("file:///C%3A/w/a.rs"), "hello"));

        docs.apply_changes(&uri("file:///c:/w/a.rs"), 2, [change(None, "goodbye")])
            .expect("the change names the same document by another spelling");
        assert_eq!(
            docs.get(&uri("FILE:///C:/w/a.rs")).unwrap().text(),
            "goodbye"
        );

        assert!(
            docs.close(&uri("file:///c%3A/w/a.rs")).is_some(),
            "the close names the same document by another spelling"
        );
        assert!(docs.get(&uri("file:///c:/w/a.rs")).is_none());
    }

    #[test]
    fn documents_is_cheap_to_clone() {
        let docs = Documents::new();
        let docs2 = docs.clone();
        let u = uri("file:///shared.txt");
        docs.open(text_item(u.clone(), "shared"));

        assert_eq!(docs2.get(&u).unwrap().text(), "shared");
    }

    #[test]
    fn position_encoding_defaults_to_utf16() {
        assert_eq!(
            Documents::new().position_encoding(),
            PositionEncoding::Utf16
        );
    }

    #[test]
    fn utf16_position_to_offset_counts_code_units() {
        // "héllo" -> h(1) é(1 utf16) l(1) l(1) o(1) = 5 UTF-16 code units on line 0.
        let (docs, u) = opened("file:///unicode.txt", "héllo\nworld");

        // 'é' starts at UTF-16 character 1 (after 'h').
        assert_eq!(docs.position_to_offset(&u, at(0, 1)), Some(1));
        // The second line starts at byte offset 7 ("héllo" = 6 bytes + '\n' = 1).
        assert_eq!(docs.position_to_offset(&u, at(1, 0)), Some(7));
    }

    #[test]
    fn utf16_offset_to_position_round_trips() {
        let (docs, u) = opened("file:///unicode.txt", "héllo\nworld");

        assert_eq!(docs.offset_to_position(&u, 1), Some(at(0, 1)));
        assert_eq!(docs.offset_to_position(&u, 7), Some(at(1, 0)));
    }

    #[test]
    fn utf8_position_is_byte_offset() {
        let (docs, u) = opened("file:///unicode.txt", "héllo\nworld");
        docs.set_position_encoding(PositionEncoding::Utf8);

        // 'é' starts at byte 1, so 'l' starts at byte 3.
        assert_eq!(docs.position_to_offset(&u, at(0, 3)), Some(3));
        assert_eq!(docs.offset_to_position(&u, 3), Some(at(0, 3)));
    }

    #[test]
    fn utf8_position_rejects_mid_codepoint_and_past_eol() {
        let (docs, u) = opened("file:///unicode.txt", "héllo\nworld");
        docs.set_position_encoding(PositionEncoding::Utf8);

        assert_eq!(
            docs.position_to_offset(&u, at(0, 2)),
            None,
            "byte 2 falls inside the two-byte 'é', so it is not a boundary"
        );
        assert_eq!(
            docs.position_to_offset(&u, at(0, 7)),
            None,
            "\"héllo\" is 6 bytes, so character 7 points past the line's content"
        );
    }

    #[test]
    fn emoji_counts_two_utf16_code_units() {
        // "a👋b" -> a(1) 👋(2 utf16) b(1) = 4 UTF-16 code units.
        let (docs, u) = opened("file:///emoji.txt", "a👋b");

        assert_eq!(
            docs.position_to_offset(&u, at(0, 3)),
            Some(5),
            "character 3 is past the emoji, at the byte after its four bytes"
        );
        assert_eq!(docs.position_to_offset(&u, at(0, 1)), Some(1));
    }

    #[test]
    fn a_change_advances_the_version_and_replaces_the_range() {
        let (docs, u) = opened("file:///change.txt", "hello world");

        docs.apply_changes(&u, 2, [change(Some(range(6, 11)), "lspf")])
            .expect("the change applies cleanly");

        let doc = docs.get(&u).unwrap();
        assert_eq!(doc.text(), "hello lspf");
        assert_eq!(doc.version(), 2);
    }

    #[test]
    fn an_omitted_range_replaces_the_whole_document() {
        let (docs, u) = opened("file:///change.txt", "hello");

        docs.apply_changes(&u, 2, [change(None, "goodbye")])
            .expect("the change applies cleanly");

        let doc = docs.get(&u).unwrap();
        assert_eq!(doc.text(), "goodbye");
        assert_eq!(doc.version(), 2);
    }

    #[test]
    fn a_reversed_range_is_rejected_without_poisoning_the_store() {
        // A range whose end precedes its start would panic `Rope::remove` while
        // the write lock is held, poisoning the store for every later access.
        let (docs, u) = opened("file:///reversed.txt", "hello world");

        assert!(
            docs.apply_changes(&u, 2, [change(Some(range(11, 6)), "x")])
                .is_err(),
            "a reversed range is an invalid request"
        );

        let doc = docs.get(&u).expect("the store is still readable");
        assert_eq!(doc.text(), "hello world");
        assert_eq!(doc.version(), 1, "a rejected change advances nothing");
    }

    #[test]
    fn a_change_to_an_unopened_document_is_rejected() {
        let docs = Documents::new();
        assert!(
            docs.apply_changes(&uri("file:///never-opened.txt"), 2, [change(None, "x")])
                .is_err(),
            "a change names a document the store must already hold"
        );
    }

    #[test]
    fn negotiation_defaults_to_utf16_when_the_client_offers_nothing() {
        let docs = Documents::new();
        assert_eq!(
            docs.negotiate_position_encoding(&InitializeParams::default()),
            PositionEncodingKind::UTF16
        );
        assert_eq!(docs.position_encoding(), PositionEncoding::Utf16);
    }

    #[test]
    fn negotiation_picks_utf8_when_the_client_offers_it() {
        let docs = Documents::new();
        assert_eq!(
            docs.negotiate_position_encoding(&offering(vec![PositionEncodingKind::UTF8])),
            PositionEncodingKind::UTF8
        );
        assert_eq!(docs.position_encoding(), PositionEncoding::Utf8);
    }

    #[test]
    fn negotiation_falls_back_to_utf16_for_utf16_only_and_unsupported_offers() {
        for offered in [PositionEncodingKind::UTF16, PositionEncodingKind::UTF32] {
            let docs = Documents::new();
            assert_eq!(
                docs.negotiate_position_encoding(&offering(vec![offered.clone()])),
                PositionEncodingKind::UTF16,
                "offering only {offered:?} leaves the LSP-mandatory default"
            );
            assert_eq!(docs.position_encoding(), PositionEncoding::Utf16);
        }
    }

    #[test]
    fn negotiation_prefers_utf8_over_utf16_when_both_are_offered() {
        let docs = Documents::new();
        // Offered UTF-16 first: lspf's own preference order decides, not the
        // client's ordering.
        assert_eq!(
            docs.negotiate_position_encoding(&offering(vec![
                PositionEncodingKind::UTF16,
                PositionEncodingKind::UTF8,
            ])),
            PositionEncodingKind::UTF8
        );
        assert_eq!(docs.position_encoding(), PositionEncoding::Utf8);
    }
}
