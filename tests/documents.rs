use std::str::FromStr;

use lspf::types::{
    DidOpenTextDocumentParams, Position, Range, TextDocumentContentChangeEvent, TextDocumentItem,
    Uri,
};
use lspf::{Context, Documents, LanguageServer, PositionEncoding};

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

#[test]
fn open_document_can_be_read_back() {
    let docs = Documents::new();
    let u = uri("file:///tmp/test.txt");
    docs.open(text_item(u.clone(), "hello world"));

    let doc = docs.get(&u).expect("document should exist");
    assert_eq!(doc.uri(), &u);
    assert_eq!(doc.language_id(), "plaintext");
    assert_eq!(doc.version(), 1);
    assert_eq!(doc.text(), "hello world");
}

#[test]
fn position_encoding_defaults_to_utf16() {
    let docs = Documents::new();
    assert_eq!(docs.position_encoding(), PositionEncoding::Utf16);
}

#[test]
fn position_encoding_can_be_set() {
    let docs = Documents::new();
    docs.set_position_encoding(PositionEncoding::Utf8);
    assert_eq!(docs.position_encoding(), PositionEncoding::Utf8);
}

#[test]
fn utf16_position_to_offset_counts_code_units() {
    // "héllo" -> h(1) é(1 utf16) l(1) l(1) o(1) = 5 UTF-16 code units on line 0.
    let docs = Documents::new();
    let u = uri("file:///unicode.txt");
    docs.open(text_item(u.clone(), "héllo\nworld"));

    // 'é' starts at UTF-16 character 1 (after 'h').
    let offset = docs
        .position_to_offset(
            &u,
            Position {
                line: 0,
                character: 1,
            },
        )
        .expect("valid position");
    assert_eq!(offset, 1);

    // The second line starts at byte offset 7 ("héllo" = 6 bytes + '\n' = 1).
    let offset = docs
        .position_to_offset(
            &u,
            Position {
                line: 1,
                character: 0,
            },
        )
        .expect("valid position");
    assert_eq!(offset, 7);
}

#[test]
fn utf16_offset_to_position_round_trips() {
    let docs = Documents::new();
    let u = uri("file:///unicode.txt");
    docs.open(text_item(u.clone(), "héllo\nworld"));

    let pos = docs.offset_to_position(&u, 1).expect("valid offset");
    assert_eq!(
        pos,
        Position {
            line: 0,
            character: 1
        }
    );

    let pos = docs.offset_to_position(&u, 7).expect("valid offset");
    assert_eq!(
        pos,
        Position {
            line: 1,
            character: 0
        }
    );
}

#[test]
fn utf8_position_is_byte_offset() {
    let docs = Documents::new();
    docs.set_position_encoding(PositionEncoding::Utf8);
    let u = uri("file:///unicode.txt");
    docs.open(text_item(u.clone(), "héllo\nworld"));

    // 'é' starts at byte 1; 'l' starts at byte 3.
    let offset = docs
        .position_to_offset(
            &u,
            Position {
                line: 0,
                character: 3,
            },
        )
        .expect("valid position");
    assert_eq!(offset, 3);

    let pos = docs.offset_to_position(&u, 3).expect("valid offset");
    assert_eq!(
        pos,
        Position {
            line: 0,
            character: 3
        }
    );
}

#[test]
fn emoji_counts_two_utf16_code_units() {
    // "a👋b" -> a(1) 👋(2 utf16) b(1) = 4 UTF-16 code units.
    let docs = Documents::new();
    let u = uri("file:///emoji.txt");
    docs.open(text_item(u.clone(), "a👋b"));

    // Position after the emoji is character 3 in UTF-16.
    let offset = docs
        .position_to_offset(
            &u,
            Position {
                line: 0,
                character: 3,
            },
        )
        .expect("valid position");
    assert_eq!(offset, 5); // byte offset after the 4-byte emoji

    // Position at the emoji start.
    let offset = docs
        .position_to_offset(
            &u,
            Position {
                line: 0,
                character: 1,
            },
        )
        .expect("valid position");
    assert_eq!(offset, 1);
}

#[test]
fn apply_incremental_change_replaces_text() {
    let docs = Documents::new();
    let u = uri("file:///change.txt");
    docs.open(text_item(u.clone(), "hello world"));

    docs.apply_incremental_change(
        &u,
        2,
        TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position {
                    line: 0,
                    character: 6,
                },
                end: Position {
                    line: 0,
                    character: 11,
                },
            }),
            range_length: None,
            text: "lspf".to_string(),
        },
    )
    .expect("change applies cleanly");

    let doc = docs.get(&u).unwrap();
    assert_eq!(doc.text(), "hello lspf");
    assert_eq!(
        doc.version(),
        2,
        "incremental change should advance version"
    );
}

#[test]
fn apply_incremental_change_rejects_reversed_range() {
    // A range whose end precedes its start must be refused, not panic the
    // store (which would poison the lock for every later access).
    let docs = Documents::new();
    let u = uri("file:///reversed.txt");
    docs.open(text_item(u.clone(), "hello world"));

    let err = docs.apply_incremental_change(
        &u,
        2,
        TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position {
                    line: 0,
                    character: 11,
                },
                end: Position {
                    line: 0,
                    character: 6,
                },
            }),
            range_length: None,
            text: "x".to_string(),
        },
    );
    assert!(err.is_err(), "reversed range should be rejected");

    // The store is still usable afterwards (lock not poisoned).
    let doc = docs.get(&u).unwrap();
    assert_eq!(doc.text(), "hello world");
    assert_eq!(doc.version(), 1, "rejected change must not advance version");
}

#[test]
fn apply_incremental_change_with_full_document_range() {
    let docs = Documents::new();
    let u = uri("file:///change.txt");
    docs.open(text_item(u.clone(), "hello"));

    // range omitted -> full document replacement
    docs.apply_incremental_change(
        &u,
        2,
        TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "goodbye".to_string(),
        },
    )
    .expect("change applies cleanly");

    let doc = docs.get(&u).unwrap();
    assert_eq!(doc.text(), "goodbye");
    assert_eq!(doc.version(), 2);
}

#[test]
fn save_returns_none_for_unknown_document() {
    let docs = Documents::new();
    assert!(docs.save(&uri("file:///never-opened.txt")).is_none());
}

#[test]
fn utf8_position_rejects_mid_codepoint_and_past_eol() {
    let docs = Documents::new();
    docs.set_position_encoding(PositionEncoding::Utf8);
    let u = uri("file:///unicode.txt");
    docs.open(text_item(u.clone(), "héllo\nworld"));

    // Byte 2 falls inside the two-byte 'é' (starts at byte 1) -> not a boundary.
    assert!(
        docs.position_to_offset(
            &u,
            Position {
                line: 0,
                character: 2
            }
        )
        .is_none(),
        "mid-codepoint byte offset must be rejected"
    );

    // "héllo" is 6 bytes; character 7 would point past the line's newline.
    assert!(
        docs.position_to_offset(
            &u,
            Position {
                line: 0,
                character: 7
            }
        )
        .is_none(),
        "offset past end-of-line content must be rejected"
    );
}

#[test]
fn close_removes_document() {
    let docs = Documents::new();
    let u = uri("file:///close.txt");
    docs.open(text_item(u.clone(), "x"));
    assert!(docs.get(&u).is_some());

    docs.close(&u);
    assert!(docs.get(&u).is_none());
}

#[test]
fn save_is_a_no_op_hook() {
    let docs = Documents::new();
    let u = uri("file:///save.txt");
    docs.open(text_item(u.clone(), "x"));
    assert!(docs.save(&u).is_some());
    assert_eq!(docs.get(&u).unwrap().text(), "x");
}

#[test]
fn documents_is_cheap_to_clone() {
    let docs = Documents::new();
    let docs2 = docs.clone();
    let u = uri("file:///shared.txt");
    docs.open(text_item(u.clone(), "shared"));

    assert_eq!(docs2.get(&u).unwrap().text(), "shared");
}

struct Mirror {
    documents: Documents,
}

impl LanguageServer for Mirror {
    fn documents(&self) -> &Documents {
        &self.documents
    }

    async fn text_document_did_open(&self, ctx: &Context, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        ctx.documents().open(params.text_document);
        assert!(self.documents().get(&uri).is_some());
    }
}

#[tokio::test]
async fn handler_sees_same_documents_via_self_and_context() {
    let u = uri("file:///mirror.txt");
    let item = text_item(u.clone(), "mirror me");
    let params = DidOpenTextDocumentParams {
        text_document: item,
    };

    let documents = Documents::new();
    let server = Mirror {
        documents: documents.clone(),
    };

    server
        .text_document_did_open(&Context::for_test_notification(documents), params)
        .await;

    assert_eq!(server.documents().get(&u).unwrap().text(), "mirror me");
}
