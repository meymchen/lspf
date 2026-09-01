use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use gen_lsp_types::{
    InitializeParams, Position, PositionEncodingKind, TextDocumentContentChangeEvent,
    TextDocumentItem, Uri,
};
use ropey::Rope;

use crate::telemetry::{Resource, ResourceAction};
use crate::uri_key::UriKey;

const DOCUMENT_COUNT_CAPACITY_EXHAUSTED: &str = "document count capacity exhausted";
const DOCUMENT_TEXT_CAPACITY_EXHAUSTED: &str = "document text capacity exhausted";

#[derive(Debug)]
pub(crate) enum DocumentMutationError {
    Capacity(crate::LspError),
    Protocol(crate::LspError),
}

fn document_text_capacity_exhausted() -> DocumentMutationError {
    DocumentMutationError::Capacity(crate::LspError::invalid_request(
        DOCUMENT_TEXT_CAPACITY_EXHAUSTED,
    ))
}

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
    version: Option<i32>,
    text: Rope,
}

impl Document {
    /// The document URI supplied by the client or file provider.
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// The client-supplied language identifier, or an empty string for a
    /// provider-loaded snapshot.
    pub fn language_id(&self) -> &str {
        &self.language_id
    }

    /// The editor-managed version, or `None` for a snapshot loaded through a
    /// [`FileProvider`](crate::FileProvider).
    pub fn version(&self) -> Option<i32> {
        self.version
    }

    pub(crate) fn provider_snapshot(uri: Uri, text: String) -> Self {
        Self {
            uri,
            language_id: String::new(),
            version: None,
            text: Rope::from_str(&text),
        }
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

    /// Apply one content change to this document's text, interpreting a
    /// partial change's `range` under `encoding`. A whole-document change
    /// replaces the complete text.
    ///
    /// Leaves the text untouched when the change is rejected, so a caller
    /// applying a batch can abandon a working copy without having corrupted
    /// anything.
    pub(crate) fn apply_change(
        &mut self,
        encoding: PositionEncoding,
        change: TextDocumentContentChangeEvent,
        max_bytes: usize,
    ) -> std::result::Result<(), DocumentMutationError> {
        let (range, text) = match change {
            TextDocumentContentChangeEvent::TextDocumentContentChangeWholeDocument(change) => {
                if change.text.len() > max_bytes {
                    return Err(document_text_capacity_exhausted());
                }
                self.text = Rope::from_str(&change.text);
                return Ok(());
            }
            TextDocumentContentChangeEvent::TextDocumentContentChangePartial(change) => {
                (change.range, change.text)
            }
        };
        if text.len() > max_bytes {
            return Err(document_text_capacity_exhausted());
        }
        let start_offset = self
            .position_to_offset(encoding, range.start)
            .ok_or_else(|| {
                DocumentMutationError::Protocol(crate::LspError::invalid_request(
                    "invalid start position",
                ))
            })?;
        let end_offset = self
            .position_to_offset(encoding, range.end)
            .ok_or_else(|| {
                DocumentMutationError::Protocol(crate::LspError::invalid_request(
                    "invalid end position",
                ))
            })?;
        // A reversed range (end before start) would panic `Rope::remove`
        // while the write lock is held, poisoning the store for every
        // later access. Reject it as an invalid request instead.
        if start_offset > end_offset {
            return Err(DocumentMutationError::Protocol(
                crate::LspError::invalid_request("range end precedes range start"),
            ));
        }
        let retained_bytes = self.text.len_bytes() - (end_offset - start_offset);
        if retained_bytes
            .checked_add(text.len())
            .is_none_or(|bytes| bytes > max_bytes)
        {
            return Err(document_text_capacity_exhausted());
        }
        let start_char = self.text.byte_to_char(start_offset);
        let end_char = self.text.byte_to_char(end_offset);
        self.text.remove(start_char..end_char);
        self.text.insert(start_char, &text);
        Ok(())
    }
}

#[derive(Debug)]
struct DocumentsInner {
    /// Identity is the normalized [`UriKey`], so equivalent spellings of one
    /// URI address one document; each [`Document`] still carries the original
    /// URI the client opened it with.
    by_uri: HashMap<UriKey, Document>,
    encoding: PositionEncoding,
    max_documents: usize,
    max_document_bytes: usize,
    document_bytes: usize,
}

impl Default for DocumentsInner {
    fn default() -> Self {
        Self {
            by_uri: HashMap::new(),
            encoding: PositionEncoding::default(),
            max_documents: crate::ResourcePolicy::default().max_documents,
            max_document_bytes: crate::ResourcePolicy::default().max_document_bytes,
            document_bytes: 0,
        }
    }
}

/// Concurrency-safe handle to every tracked [`Document`], owned by the
/// connection's protocol engine (ADR 0003, ADR 0018).
///
/// Cheap to clone: all copies share the same `Arc<RwLock<...>>`. The type is
/// crate-private on purpose — user code never constructs a store, never holds
/// one in its state, and never mutates one. Mutations happen only through the
/// engine's built-in doc-sync handling (`open`, `apply_changes`, `close`), and
/// handlers read through the [`DocumentsView`] the [`ServerContext`] hands them.
///
/// [`ServerContext`]: crate::ServerContext
#[derive(Debug, Clone)]
pub(crate) struct Documents {
    inner: Arc<RwLock<DocumentsInner>>,
    trace: crate::telemetry::ConnectionTrace,
}

impl Default for Documents {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(DocumentsInner::default())),
            trace: crate::telemetry::ConnectionTrace::new(),
        }
    }
}

impl Documents {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_resource_policy(
        policy: crate::ResourcePolicy,
        trace: crate::telemetry::ConnectionTrace,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(DocumentsInner {
                max_documents: policy.max_documents,
                max_document_bytes: policy.max_document_bytes,
                ..DocumentsInner::default()
            })),
            trace,
        }
    }

    /// Open or replace a document in the store.
    pub(crate) fn open(
        &self,
        item: TextDocumentItem,
    ) -> std::result::Result<(), DocumentMutationError> {
        let mut inner = self.inner.write().unwrap();
        let key = UriKey::new(&item.uri);
        if !inner.by_uri.contains_key(&key) && inner.by_uri.len() >= inner.max_documents {
            self.trace.resource_budget(
                Resource::Documents,
                ResourceAction::Reject,
                inner.by_uri.len(),
                inner.max_documents,
                Some((inner.document_bytes, inner.max_document_bytes)),
            );
            return Err(DocumentMutationError::Capacity(
                crate::LspError::invalid_request(DOCUMENT_COUNT_CAPACITY_EXHAUSTED),
            ));
        }

        let replaced_bytes = inner
            .by_uri
            .get(&key)
            .map(|document| document.text.len_bytes())
            .unwrap_or(0);
        let retained_bytes = inner.document_bytes - replaced_bytes;
        let Some(document_bytes) = retained_bytes.checked_add(item.text.len()) else {
            self.trace.resource_budget(
                Resource::Documents,
                ResourceAction::Reject,
                inner.by_uri.len(),
                inner.max_documents,
                Some((inner.document_bytes, inner.max_document_bytes)),
            );
            return Err(document_text_capacity_exhausted());
        };
        if document_bytes > inner.max_document_bytes {
            self.trace.resource_budget(
                Resource::Documents,
                ResourceAction::Reject,
                inner.by_uri.len(),
                inner.max_documents,
                Some((inner.document_bytes, inner.max_document_bytes)),
            );
            return Err(document_text_capacity_exhausted());
        }

        inner.by_uri.insert(
            key,
            Document {
                uri: item.uri,
                language_id: item.language_id.as_str().to_owned(),
                version: Some(item.version),
                text: Rope::from_str(&item.text),
            },
        );
        inner.document_bytes = document_bytes;
        self.trace.resource_budget(
            Resource::Documents,
            ResourceAction::Admit,
            inner.by_uri.len(),
            inner.max_documents,
            Some((inner.document_bytes, inner.max_document_bytes)),
        );
        Ok(())
    }

    /// Read a snapshot of a document by URI.
    pub(crate) fn get(&self, uri: &Uri) -> Option<Document> {
        let inner = self.inner.read().unwrap();
        inner.by_uri.get(&UriKey::new(uri)).cloned()
    }

    /// Remove a document from the store. Returns the removed document, if any.
    pub(crate) fn close(&self, uri: &Uri) -> Option<Document> {
        let mut inner = self.inner.write().unwrap();
        let removed = inner.by_uri.remove(&UriKey::new(uri));
        if let Some(document) = &removed {
            inner.document_bytes -= document.text.len_bytes();
            self.trace.resource_budget(
                Resource::Documents,
                ResourceAction::Release,
                inner.by_uri.len(),
                inner.max_documents,
                Some((inner.document_bytes, inner.max_document_bytes)),
            );
        }
        removed
    }

    /// Release every connection-owned document and its byte accounting.
    pub(crate) fn clear(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.by_uri = HashMap::new();
        inner.document_bytes = 0;
        self.trace.resource_budget(
            Resource::Documents,
            ResourceAction::Release,
            0,
            inner.max_documents,
            Some((0, inner.max_document_bytes)),
        );
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
    ) -> std::result::Result<(), DocumentMutationError> {
        let mut inner = self.inner.write().unwrap();
        let encoding = inner.encoding;
        let key = UriKey::new(uri);
        let document = inner.by_uri.get(&key).cloned().ok_or_else(|| {
            DocumentMutationError::Protocol(crate::LspError::invalid_request("document not found"))
        })?;
        let retained_bytes = inner.document_bytes - document.text.len_bytes();
        let available_document_bytes = inner.max_document_bytes - retained_bytes;

        // `Rope` clones share their nodes, so the working copy costs little
        // more than the edits it actually makes.
        let mut updated = document;
        for change in changes {
            if let Err(error) = updated.apply_change(encoding, change, available_document_bytes) {
                if matches!(error, DocumentMutationError::Capacity(_)) {
                    self.trace.resource_budget(
                        Resource::Documents,
                        ResourceAction::Reject,
                        inner.by_uri.len(),
                        inner.max_documents,
                        Some((inner.document_bytes, inner.max_document_bytes)),
                    );
                }
                return Err(error);
            }
        }
        updated.version = Some(version);
        inner.document_bytes = retained_bytes + updated.text.len_bytes();
        inner.by_uri.insert(key, updated);
        self.trace.resource_budget(
            Resource::Documents,
            ResourceAction::Update,
            inner.by_uri.len(),
            inner.max_documents,
            Some((inner.document_bytes, inner.max_document_bytes)),
        );
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
/// [`ServerContext::documents`](crate::ServerContext::documents) hands to user code
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

    use gen_lsp_types::{GeneralClientCapabilities, Range};

    use super::*;

    fn uri(s: &str) -> Uri {
        Uri::from_str(s).unwrap()
    }

    fn text_item(uri: Uri, text: &str) -> TextDocumentItem {
        TextDocumentItem {
            uri,
            language_id: "plaintext".into(),
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
        docs.open(text_item(u.clone(), text))
            .expect("the default policy accepts the test document");
        (docs, u)
    }

    /// One content change, as `didChange` delivers it.
    fn change(range: Option<Range>, text: &str) -> TextDocumentContentChangeEvent {
        match range {
            Some(range) => {
                gen_lsp_types::TextDocumentContentChangePartial::new(range, None, text.to_string())
                    .into()
            }
            None => gen_lsp_types::TextDocumentContentChangeWholeDocument {
                text: text.to_string(),
            }
            .into(),
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
        assert_eq!(doc.version(), Some(1));
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
        docs.open(text_item(original.clone(), "fn main() {}"))
            .expect("the default policy accepts the test document");

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
        docs.open(text_item(uri("file:///home/Foo.rs"), "a"))
            .expect("the default policy accepts the test document");
        assert!(
            docs.get(&uri("file:///home/foo.rs")).is_none(),
            "ordinary path case is not normalized"
        );
    }

    #[test]
    fn change_and_close_resolve_through_the_same_normalized_key() {
        let docs = Documents::new();
        docs.open(text_item(uri("file:///C%3A/w/a.rs"), "hello"))
            .expect("the default policy accepts the test document");

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
        docs.open(text_item(u.clone(), "shared"))
            .expect("the default policy accepts the test document");

        assert_eq!(docs2.get(&u).unwrap().text(), "shared");
    }

    #[test]
    fn separate_document_stores_keep_same_uri_overlays_isolated() {
        // Mirrors gopls's simultaneous-editor regression at the framework
        // boundary: each connection owns its own overlay for the same URI.
        let first = Documents::new();
        let second = Documents::new();
        let u = uri("file:///shared.txt");
        first
            .open(text_item(u.clone(), "first editor"))
            .expect("the default policy accepts the first test document");
        second
            .open(text_item(u.clone(), "second editor"))
            .expect("the default policy accepts the second test document");

        first
            .apply_changes(&u, 2, [change(None, "first editor changed")])
            .expect("the first editor changes its own overlay");
        second.close(&u);

        let remaining = first
            .get(&u)
            .expect("closing the second overlay is isolated");
        assert_eq!(remaining.text(), "first editor changed");
        assert_eq!(remaining.version(), Some(2));
        assert!(second.get(&u).is_none());
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
    fn invalid_utf16_edit_positions_are_rejected_without_changing_the_document() {
        // Adapted from clangd's PositionToOffset and invalid edit endpoint
        // tests. UTF-16 position 2 bisects the emoji's surrogate pair.
        let (docs, u) = opened("file:///invalid-utf16.txt", "a👋b");
        let invalid_ranges = [
            Range {
                start: at(0, 2),
                end: at(0, 2),
            },
            Range {
                start: at(0, 5),
                end: at(0, 5),
            },
            Range {
                start: at(1, 0),
                end: at(1, 0),
            },
        ];

        for invalid_range in invalid_ranges {
            assert!(
                docs.apply_changes(&u, 2, [change(Some(invalid_range), "x")])
                    .is_err(),
                "an invalid UTF-16 endpoint must reject the whole change"
            );
            let doc = docs.get(&u).expect("the rejected edit keeps the document");
            assert_eq!(doc.text(), "a👋b");
            assert_eq!(doc.version(), Some(1));
        }

        assert_eq!(
            docs.position_to_offset(&u, at(0, 3)),
            Some(5),
            "a later valid lookup still works"
        );
    }

    #[test]
    fn utf8_and_utf16_positions_round_trip_complex_unicode_across_lines() {
        // Covers a mixed-width sample: composed and decomposed text,
        // BMP characters, a surrogate pair, and a line boundary.
        let text = "äa\u{0308}錯誤😋\näa\u{0308}錯誤😋";
        let (docs, u) = opened("file:///position-round-trip.txt", text);

        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            docs.set_position_encoding(encoding);
            let mut line_start = 0;

            for (line_index, line) in text.split('\n').enumerate() {
                let mut character = 0;
                for (byte_in_line, ch) in line.char_indices() {
                    let position = at(line_index as u32, character);
                    let offset = line_start + byte_in_line;
                    assert_eq!(docs.position_to_offset(&u, position), Some(offset));
                    assert_eq!(docs.offset_to_position(&u, offset), Some(position));
                    character += match encoding {
                        PositionEncoding::Utf8 => ch.len_utf8() as u32,
                        PositionEncoding::Utf16 => ch.len_utf16() as u32,
                    };
                }

                let line_end = at(line_index as u32, character);
                assert_eq!(
                    docs.position_to_offset(&u, line_end),
                    Some(line_start + line.len())
                );
                assert_eq!(
                    docs.offset_to_position(&u, line_start + line.len()),
                    Some(line_end)
                );
                line_start += line.len() + 1;
            }
        }
    }

    #[test]
    fn positions_use_line_content_before_crlf_and_lf_endings() {
        let (docs, u) = opened("file:///line-endings.txt", "x\r\ny\n");

        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            docs.set_position_encoding(encoding);
            assert_eq!(docs.position_to_offset(&u, at(0, 1)), Some(1));
            assert_eq!(docs.offset_to_position(&u, 1), Some(at(0, 1)));
            assert_eq!(docs.position_to_offset(&u, at(1, 0)), Some(3));
            assert_eq!(docs.offset_to_position(&u, 3), Some(at(1, 0)));
            assert_eq!(docs.position_to_offset(&u, at(1, 1)), Some(4));
            assert_eq!(docs.position_to_offset(&u, at(2, 0)), Some(5));
        }
    }

    #[test]
    fn a_change_advances_the_version_and_replaces_the_range() {
        let (docs, u) = opened("file:///change.txt", "hello world");

        docs.apply_changes(&u, 2, [change(Some(range(6, 11)), "lspf")])
            .expect("the change applies cleanly");

        let doc = docs.get(&u).unwrap();
        assert_eq!(doc.text(), "hello lspf");
        assert_eq!(doc.version(), Some(2));
    }

    #[test]
    fn successful_changes_record_no_op_and_non_monotonic_versions() {
        // Adapted from clangd's DraftStore.Versions regression: the client
        // owns the version value, even when text is unchanged or it decreases.
        let docs = Documents::new();
        let u = uri("file:///versions.txt");
        docs.open(TextDocumentItem {
            uri: u.clone(),
            language_id: "plaintext".into(),
            version: 25,
            text: "contents".to_string(),
        })
        .expect("the default policy accepts the test document");

        docs.apply_changes(&u, 27, [change(None, "contents")])
            .expect("a no-op replacement still records its version");
        let no_op = docs.get(&u).unwrap();
        assert_eq!(no_op.text(), "contents");
        assert_eq!(no_op.version(), Some(27));

        docs.apply_changes(&u, 7, [change(None, "new contents")])
            .expect("the store accepts the client's non-monotonic version");
        let regressed = docs.get(&u).unwrap();
        assert_eq!(regressed.text(), "new contents");
        assert_eq!(regressed.version(), Some(7));
    }

    #[test]
    fn a_change_batch_reinterprets_later_ranges_after_unicode_multiline_edits() {
        // Ported from rust-analyzer's `test_apply_document_changes`: the
        // second range is expressed against the text produced by the first.
        let (docs, u) = opened("file:///sequential-edits.txt", "a\nb");
        let changes = [
            change(
                Some(Range {
                    start: at(0, 1),
                    end: at(1, 0),
                }),
                "\nțc",
            ),
            change(
                Some(Range {
                    start: at(0, 1),
                    end: at(1, 1),
                }),
                "d",
            ),
        ];

        docs.apply_changes(&u, 2, changes)
            .expect("each range is interpreted against the preceding edit");

        let doc = docs.get(&u).unwrap();
        assert_eq!(doc.text(), "adcb");
        assert_eq!(doc.version(), Some(2));
    }

    #[test]
    fn an_incremental_change_can_insert_into_an_empty_document() {
        let (docs, u) = opened("file:///empty.txt", "");

        docs.apply_changes(&u, 2, [change(Some(range(0, 0)), "f")])
            .expect("the insertion applies at the empty document's only position");

        let doc = docs.get(&u).unwrap();
        assert_eq!(doc.text(), "f");
        assert_eq!(doc.version(), Some(2));
    }

    #[test]
    fn an_incremental_change_can_insert_after_a_trailing_newline() {
        let (docs, u) = opened("file:///eof.txt", "first\nsecond\n");
        let eof = Range {
            start: at(2, 0),
            end: at(2, 0),
        };

        docs.apply_changes(&u, 2, [change(Some(eof), "third")])
            .expect("the trailing newline exposes an empty final line");

        let doc = docs.get(&u).unwrap();
        assert_eq!(doc.text(), "first\nsecond\nthird");
        assert_eq!(doc.version(), Some(2));
    }

    #[test]
    fn an_omitted_range_replaces_the_whole_document() {
        let (docs, u) = opened("file:///change.txt", "hello");

        docs.apply_changes(&u, 2, [change(None, "goodbye")])
            .expect("the change applies cleanly");

        let doc = docs.get(&u).unwrap();
        assert_eq!(doc.text(), "goodbye");
        assert_eq!(doc.version(), Some(2));
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
        assert_eq!(doc.version(), Some(1), "a rejected change advances nothing");
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
