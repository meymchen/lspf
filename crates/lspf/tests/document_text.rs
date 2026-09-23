//! Text access through snapshots obtained by a real public Server handler.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use lspf::testing::ServerJourney;
use lspf::types::{InitializeParams, Uri};
use lspf::{
    CancellationToken, Document, LspError, MemoryFileProvider, Outcome, RawMessage, RequestId,
    Server, ServerContext,
};
use serde_json::{Value, json};

const URI: &str = "file:///text.txt";

enum Query {}
impl lspf::types::request::Request for Query {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "test/text";
}

#[derive(Default)]
struct State {
    retained: Mutex<Option<Document>>,
}

async fn query(
    state: Arc<State>,
    ctx: ServerContext,
    params: Value,
    _: CancellationToken,
) -> Result<Value, LspError> {
    let uri: Uri = params["uri"].as_str().unwrap_or(URI).parse().unwrap();
    let document = if params["retained"] == true {
        state.retained.lock().unwrap().clone().unwrap()
    } else if params["provider"] == true {
        ctx.workspace().text_document(&uri).await.unwrap()
    } else {
        ctx.documents().get(&uri).unwrap()
    };
    if params["retain"] == true {
        *state.retained.lock().unwrap() = Some(document.clone());
    }
    let line_range = document.line_range(params["line"].as_u64().unwrap_or(0) as u32);
    let line = line_range.map(|range| document.text(Some(range)));
    let mut result =
        json!({"line": line, "text": document.text(None), "version": document.version()});
    if params["lineMetadata"] == true {
        result["lineCount"] = json!(document.line_count());
        result["lineRange"] = json!(line_range);
        result["encoding"] = json!(match document.position_encoding() {
            lspf::PositionEncoding::Utf8 => "utf-8",
            lspf::PositionEncoding::Utf16 => "utf-16",
            lspf::PositionEncoding::Utf32 => "utf-32",
        });
    }
    if let Some(range) = params.get("range") {
        let range = serde_json::from_value(range.clone()).unwrap();
        result["selection"] = json!(document.text(Some(range)));
    }
    if let Some(position) = params.get("position") {
        let position = serde_json::from_value(position.clone()).unwrap();
        let predicate = |ch: char| match params["predicate"].as_str() {
            Some("unicode") => {
                ch.is_alphanumeric() || matches!(ch, '_' | '-' | '\u{301}' | '\u{1f600}')
            }
            Some("all") => true,
            _ => ch.is_ascii_alphanumeric() || ch == '_',
        };
        result["word"] = json!(document.word_at_position(position, predicate));
    }
    if params["coordinates"] == true {
        let position = serde_json::from_value(params["position"].clone()).unwrap();
        result["offsetAtPosition"] = json!(document.position_to_offset(position));
        result["positionAtOffset"] =
            json!(document.offset_to_position(params["offset"].as_u64().unwrap() as usize));
    }
    if params["metadata"] == true {
        result["uri"] = json!(document.uri());
        result["languageId"] = json!(document.language_id());
        result["live"] = json!(ctx.documents().get(&uri).is_some());
    }
    Ok(result)
}

fn notify(journey: &mut ServerJourney, method: &'static str, params: Value) {
    journey
        .peer()
        .send(RawMessage::Notification {
            method: method.into(),
            params: Bytes::from(serde_json::to_vec(&params).unwrap()),
        })
        .unwrap();
}

async fn read(journey: &mut ServerJourney, params: Value) -> Value {
    journey
        .peer()
        .send(RawMessage::Request {
            id: RequestId::Number(10),
            method: "test/text".into(),
            params: Bytes::from(serde_json::to_vec(&params).unwrap()),
        })
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), journey.peer().recv())
        .await
        .unwrap()
        .unwrap();
    let RawMessage::Response { id, result } = response else {
        panic!("expected response: {response:?}")
    };
    assert_eq!(id, RequestId::Number(10));
    serde_json::from_slice(&result.unwrap()).unwrap()
}

async fn opened(text: &str, encoding: Option<&str>) -> ServerJourney {
    let server = server(MemoryFileProvider::new());
    let params: InitializeParams = serde_json::from_value(json!({
        "capabilities": encoding.map_or(json!({}), |encoding| json!({"general": {"positionEncodings": [encoding]}})),
    })).unwrap();
    let mut journey = ServerJourney::start_with(server, params).await.unwrap();
    notify(
        &mut journey,
        "textDocument/didOpen",
        json!({"textDocument": {
            "uri": URI, "languageId": "text", "version": 7, "text": text,
        }}),
    );
    journey
}

#[tokio::test]
async fn lines_preserve_content_and_omit_whole_terminators() {
    for (text, expected) in [
        ("", vec![""]),
        ("one  \n\nlast", vec!["one  ", "", "last"]),
        ("one\r\n\r\nlast\r\n", vec!["one", "", "last", ""]),
        ("one\r\rlast\r", vec!["one", "", "last", ""]),
    ] {
        let mut journey = opened(text, None).await;
        for (line, expected) in expected.iter().enumerate() {
            let result = read(&mut journey, json!({"line": line})).await;
            assert_eq!(
                result,
                json!({"line": expected, "text": text, "version": 7})
            );
        }
        for line in [expected.len() as u32, u32::MAX] {
            assert_eq!(
                read(&mut journey, json!({"line": line})).await["line"],
                Value::Null
            );
            assert_eq!(
                read(&mut journey, json!({"line": 0})).await["line"],
                expected[0]
            );
        }
        assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });
    }
}

fn range(start_line: u32, start: u32, end_line: u32, end: u32) -> Value {
    json!({"start": {"line": start_line, "character": start}, "end": {"line": end_line, "character": end}})
}

#[tokio::test]
async fn line_ranges_use_negotiated_columns_and_counts_include_trailing_empty_lines() {
    for (encoding, end) in [("utf-8", 11), ("utf-16", 6), ("utf-32", 5)] {
        let mut journey = opened("a中😀e\u{301}\r\n\nlast\r", Some(encoding)).await;
        for (line, last_column, text) in [
            (0, end, "a中😀e\u{301}"),
            (1, 0, ""),
            (2, 4, "last"),
            (3, 0, ""),
        ] {
            let result = read(&mut journey, json!({"line": line, "lineMetadata": true})).await;
            assert_eq!(result["lineCount"], 4);
            assert_eq!(result["lineRange"], range(line, 0, line, last_column));
            assert_eq!(result["line"], text);
            assert_eq!(result["text"], "a中😀e\u{301}\r\n\nlast\r");
        }
        for line in [4, u32::MAX] {
            let result = read(&mut journey, json!({"line": line, "lineMetadata": true})).await;
            assert_eq!(result["lineCount"], 4);
            assert_eq!(result["lineRange"], Value::Null);
            assert_eq!(result["line"], Value::Null);
        }
        journey.finish().await.unwrap();
    }
    let mut empty = opened("", None).await;
    let result = read(&mut empty, json!({"lineMetadata": true})).await;
    assert_eq!(result["lineCount"], 1);
    assert_eq!(result["lineRange"], range(0, 0, 0, 0));
    assert_eq!(result["text"], "");
    empty.finish().await.unwrap();
}

#[tokio::test]
async fn snapshots_keep_their_connections_encoding_for_all_coordinate_queries() {
    let mut connections = Vec::new();
    for (encoding, start, end) in [("utf-8", 5, 9), ("utf-16", 3, 7), ("utf-32", 2, 6)] {
        connections.push((
            opened("😀 word\r\n", Some(encoding)).await,
            encoding,
            start,
            end,
        ));
    }
    // Query after all three connections have negotiated different encodings.
    for (mut journey, encoding, start, end) in connections {
        let result = read(
            &mut journey,
            json!({
                "lineMetadata":true,"coordinates":true,"offset":5,
                "position":position(0,start),"range":range(0,start,0,end),
            }),
        )
        .await;
        assert_eq!(result["encoding"], encoding);
        assert_eq!(result["lineRange"], range(0, 0, 0, end));
        assert_eq!(result["lineCount"], 2);
        assert_eq!(result["selection"], "word");
        assert_eq!(result["word"], json!(["word", range(0, start, 0, end)]));
        assert_eq!(result["offsetAtPosition"], 5);
        assert_eq!(result["positionAtOffset"], position(0, start));
        journey.finish().await.unwrap();
    }
}

#[tokio::test]
async fn selections_use_negotiated_coordinates_and_preserve_source_characters() {
    // Corresponding boundaries in "a中😀e\u{301}": bytes, UTF-16 units, scalars.
    for (encoding, cjk_end, emoji_end, end) in [
        (Some("utf-8"), 4, 8, 11),
        (Some("utf-16"), 2, 4, 6),
        (Some("utf-32"), 2, 3, 5),
        (None, 2, 4, 6),
    ] {
        for ending in ["\n", "\r\n", "\r"] {
            let text = format!("a\u{4e2d}\u{1f600}e\u{301}{ending}next{ending}");
            let mut journey = opened(&text, encoding).await;
            for (selection, expected) in [
                (range(0, 0, 0, 1), "a".to_owned()),
                (range(0, 1, 0, cjk_end), "\u{4e2d}".to_owned()),
                (range(0, cjk_end, 0, emoji_end), "\u{1f600}".to_owned()),
                (range(0, emoji_end, 0, end), "e\u{301}".to_owned()),
                (range(0, end, 1, 0), ending.to_owned()),
                (range(0, emoji_end, 1, 2), format!("e\u{301}{ending}ne")),
                (range(0, end, 0, end), String::new()),
                (range(2, 0, 2, 0), String::new()),
            ] {
                let result = read(&mut journey, json!({"range": selection})).await;
                assert_eq!(result["selection"], expected, "{encoding:?}, {selection}");
                assert_eq!(result["text"], text);
                assert_eq!(result["version"], 7);
            }
            journey.finish().await.unwrap();
        }
        let mut empty = opened("", encoding).await;
        assert_eq!(
            read(&mut empty, json!({"range": range(0,0,0,0)})).await["selection"],
            ""
        );
        empty.finish().await.unwrap();
    }
}

fn position(line: u32, character: u32) -> Value {
    json!({"line": line, "character": character})
}

#[tokio::test]
async fn words_use_caller_policy_and_prefer_the_character_to_the_right() {
    for (encoding, start, end) in [
        (Some("utf-8"), 8, 13),
        (Some("utf-16"), 4, 9),
        (Some("utf-32"), 3, 8),
        (None, 4, 9),
    ] {
        let mut journey = opened("\u{4e2d}\u{1f600} alpha,  beta\r\nlast", encoding).await;
        for cursor in [start, start + 2, end] {
            assert_eq!(
                read(&mut journey, json!({"position": position(0,cursor)})).await["word"],
                json!(["alpha", range(0, start, 0, end)])
            );
        }
        // At punctuation directly after a word the left run wins; a gap does not search back.
        for cursor in [0, end + 1, end + 2] {
            assert_eq!(
                read(&mut journey, json!({"position": position(0,cursor)})).await["word"],
                Value::Null
            );
        }
        assert_eq!(
            read(&mut journey, json!({"position": position(1,0)})).await["word"],
            json!(["last", range(1, 0, 1, 4)])
        );
        assert_eq!(
            read(&mut journey, json!({"position": position(1,4)})).await["word"],
            json!(["last", range(1, 0, 1, 4)])
        );
        journey.finish().await.unwrap();
    }
    for (encoding, end) in [("utf-8", 12), ("utf-16", 7), ("utf-32", 6)] {
        let mut journey = opened("a-\u{4e2d}\u{1f600}e\u{301}\n\nnext", Some(encoding)).await;
        assert_eq!(
            read(
                &mut journey,
                json!({"position":position(0,1),"predicate":"unicode"})
            )
            .await["word"],
            json!(["a-\u{4e2d}\u{1f600}e\u{301}", range(0, 0, 0, end)])
        );
        assert_eq!(
            read(
                &mut journey,
                json!({"position":position(0,end),"predicate":"all"})
            )
            .await["word"],
            json!(["a-\u{4e2d}\u{1f600}e\u{301}", range(0, 0, 0, end)])
        );
        assert_eq!(
            read(
                &mut journey,
                json!({"position":position(1,0),"predicate":"all"})
            )
            .await["word"],
            Value::Null
        );
        journey.finish().await.unwrap();
    }
}

fn server(provider: MemoryFileProvider) -> Server<State> {
    use lspf::types::{NotebookDocumentFilterWithNotebook, NotebookDocumentSyncOptions};
    Server::builder(State::default())
        .file_provider(provider)
        .notebook_document_sync(NotebookDocumentSyncOptions::new(
            vec![NotebookDocumentFilterWithNotebook::new("test".into(), None).into()],
            None,
        ))
        .request::<Query, _, _>(query)
        .build()
        .unwrap()
}

#[tokio::test]
async fn invalid_text_ranges_return_the_whole_snapshot_without_mutation() {
    for (encoding, end, split) in [
        (Some("utf-8"), 8, vec![2, 3, 5, 6, 7]),
        (Some("utf-16"), 4, vec![3]),
        (Some("utf-32"), 3, vec![]),
        (None, 4, vec![3]),
    ] {
        let text = "a\u{4e2d}\u{1f600}\r\nb\n";
        let mut journey = opened(text, encoding).await;
        let mut invalid = vec![
            position(3, 0),
            position(u32::MAX, 0),
            position(0, u32::MAX),
            position(0, end + 1),
            position(0, end + 2),
            position(1, 2),
            position(2, 1),
        ];
        invalid.extend(split.into_iter().map(|column| position(0, column)));
        for point in invalid {
            for selection in [
                json!({"start":point,"end":point}),
                json!({"start":position(0,0),"end":point}),
                json!({"start":point,"end":position(2,0)}),
            ] {
                let result = read(
                    &mut journey,
                    json!({"range":selection,"position":point,"predicate":"all"}),
                )
                .await;
                assert_eq!(result["selection"], text, "{encoding:?} {selection}");
                assert_eq!(result["word"], Value::Null, "{encoding:?} {point}");
                assert_eq!(result["text"], text);
                assert_eq!(result["version"], 7);
                assert_eq!(
                    read(&mut journey, json!({"range":range(0,0,0,1)})).await["selection"],
                    "a"
                );
            }
        }
        for selection in [range(0, 1, 0, 0), range(1, 0, 0, end)] {
            assert_eq!(
                read(&mut journey, json!({"range":selection})).await["selection"],
                text
            );
            assert_eq!(
                read(&mut journey, json!({"range":range(0,end,0,end)})).await["selection"],
                ""
            );
        }
        journey.finish().await.unwrap();
    }
}

#[tokio::test]
async fn retained_snapshot_survives_changes_and_close() {
    for (encoding, start) in [("utf-8", 5), ("utf-16", 3), ("utf-32", 2)] {
        let original = "\u{1f600} old\r\n";
        let mut journey = opened(original, Some(encoding)).await;
        let request = json!({"range":range(0,start,0,start+3),"position":position(0,start+1),"metadata":true,"lineMetadata":true});
        let mut retain = request.clone();
        retain["retain"] = json!(true);
        let before = read(&mut journey, retain).await;
        assert_eq!(before["encoding"], encoding);
        assert_eq!(before["selection"], "old");
        assert_eq!(
            before["word"],
            json!(["old", range(0, start, 0, start + 3)])
        );
        assert_eq!(before["uri"], URI);
        assert_eq!(before["languageId"], "text");
        assert_eq!(before["lineCount"], 2);
        assert_eq!(before["lineRange"], range(0, 0, 0, start + 3));
        notify(
            &mut journey,
            "textDocument/didChange",
            json!({
                "textDocument":{"uri":URI,"version":8},
                "contentChanges":[{"range":range(0,start,0,start+3),"text":"newer\nsuffix"}],
            }),
        );
        let current = read(&mut journey, request.clone()).await;
        assert_eq!(current["encoding"], encoding);
        assert_eq!(current["selection"], "new");
        assert_eq!(
            current["word"],
            json!(["newer", range(0, start, 0, start + 5)])
        );
        assert_eq!(current["lineCount"], 3);
        assert_eq!(current["lineRange"], range(0, 0, 0, start + 5));
        assert_eq!(current["version"], 8);
        let invalid_range = range(u32::MAX, 0, u32::MAX, 0);
        assert_eq!(
            read(&mut journey, json!({"range":invalid_range})).await["selection"],
            "\u{1f600} newer\nsuffix\r\n"
        );
        let retained_fallback = json!({"retained":true,"range":invalid_range});
        assert_eq!(
            read(&mut journey, retained_fallback.clone()).await["selection"],
            original
        );
        let mut retained = request;
        retained["retained"] = json!(true);
        assert_eq!(read(&mut journey, retained.clone()).await, before);
        notify(
            &mut journey,
            "textDocument/didClose",
            json!({"textDocument":{"uri":URI}}),
        );
        let after = read(&mut journey, retained).await;
        let mut closed = before;
        closed["live"] = json!(false);
        assert_eq!(after, closed);
        assert_eq!(
            read(&mut journey, retained_fallback).await["selection"],
            original
        );
        journey.finish().await.unwrap();
    }
}

#[tokio::test]
async fn provider_and_notebook_snapshots_share_the_same_text_helpers() {
    for (encoding, start) in [("utf-8", 5), ("utf-16", 3), ("utf-32", 2)] {
        let provider = MemoryFileProvider::new();
        provider.insert(URI.parse::<Uri>().unwrap(), "\u{1f600} stored\r\n");
        let params: InitializeParams = serde_json::from_value(json!({
            "capabilities":{"general":{"positionEncodings":[encoding]}}
        }))
        .unwrap();
        let mut journey = ServerJourney::start_with(server(provider), params)
            .await
            .unwrap();
        let result = read(
            &mut journey,
            json!({"provider":true,"metadata":true,"lineMetadata":true,
        "range":range(0,start,0,start+6),"position":position(0,start+1)}),
        )
        .await;
        assert_eq!(
            result,
            json!({"line":"\u{1f600} stored","text":"\u{1f600} stored\r\n",
        "version":null,"uri":URI,"languageId":"","live":false,
        "selection":"stored","word":["stored",range(0,start,0,start+6)],
        "lineCount":2,"lineRange":range(0,0,0,start+6),"encoding":encoding})
        );
        assert_eq!(
            read(
                &mut journey,
                json!({"provider":true,"range":range(9,0,9,0)})
            )
            .await["selection"],
            "\u{1f600} stored\r\n"
        );
        let cell = "file:///book.ipynb#cell";
        notify(
            &mut journey,
            "notebookDocument/didOpen",
            json!({
                "notebookDocument":{"uri":"file:///book.ipynb","notebookType":"test","version":1,
                    "cells":[{"kind":2,"document":cell}]},
                "cellTextDocuments":[{"uri":cell,"languageId":"rust","version":3,"text":"\u{1f600} cell\r\n"}],
            }),
        );
        let result = read(
            &mut journey,
            json!({"uri":cell,"metadata":true,"lineMetadata":true,
        "range":range(0,start,0,start+4),"position":position(0,start+2)}),
        )
        .await;
        assert_eq!(
            result,
            json!({"line":"\u{1f600} cell","text":"\u{1f600} cell\r\n",
        "version":3,"uri":cell,"languageId":"rust","live":true,
        "selection":"cell","word":["cell",range(0,start,0,start+4)],
        "lineCount":2,"lineRange":range(0,0,0,start+4),"encoding":encoding})
        );
        assert_eq!(
            read(&mut journey, json!({"uri":cell,"range":range(9,0,9,0)})).await["selection"],
            "\u{1f600} cell\r\n"
        );
        journey.finish().await.unwrap();
    }
}

#[tokio::test]
async fn partial_reads_preserve_long_lines_and_multiline_fragments() {
    let word = "\u{4e2d}".repeat(2000);
    let line = format!("\u{1f600} {word}");
    let text = format!("{}\r\n{line}\r\nend", "unrelated\n".repeat(2000));
    let mut journey = opened(&text, Some("utf-16")).await;
    let result = read(
        &mut journey,
        json!({
            "line":2001,"position":position(2001,1000),"predicate":"unicode",
            "range":range(2001,3,2002,2),
        }),
    )
    .await;
    assert_eq!(result["line"], line);
    assert_eq!(result["word"], json!([word, range(2001, 3, 2001, 2003)]));
    assert_eq!(result["selection"], format!("{word}\r\nen"));
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn words_and_lines_respect_all_terminators_in_the_existing_coordinate_model() {
    for ending in ["\u{b}", "\u{c}", "\u{85}", "\u{2028}", "\u{2029}"] {
        for encoding in ["utf-8", "utf-16", "utf-32"] {
            let text = format!("a{ending}b{ending}");
            let mut journey = opened(&text, Some(encoding)).await;
            let result = read(
                &mut journey,
                json!({
                    "position":position(0,0),"predicate":"all","range":range(0,1,1,0),
                }),
            )
            .await;
            assert_eq!(result["line"], "a");
            assert_eq!(result["word"], json!(["a", range(0, 0, 0, 1)]));
            assert_eq!(result["selection"], ending);
            assert_eq!(read(&mut journey, json!({"line":2})).await["line"], "");
            let result = read(
                &mut journey,
                json!({
                    "position":position(0,2),"predicate":"all","range":range(0,2,0,2),
                }),
            )
            .await;
            assert_eq!(result["word"], Value::Null);
            assert_eq!(result["selection"], text);
            journey.finish().await.unwrap();
        }
    }
}

#[tokio::test]
async fn local_queries_after_a_long_unicode_prefix_keep_exact_encoded_boundaries() {
    // Each repeated prefix has 11 UTF-8 bytes, 6 UTF-16 units, and 5 scalars.
    let prefix = "\u{4e2d}\u{1f600}e\u{301} ".repeat(600);
    let word = "a-\u{4e2d}\u{1f600}e\u{301}";
    let text = format!("first\r\n{prefix}{word} {}\r\n", "suffix ".repeat(600));
    for (encoding, start, end, split) in [
        ("utf-8", 6600, 6612, Some(6603)),
        ("utf-16", 3600, 3607, Some(3604)),
        ("utf-32", 3000, 3006, None),
    ] {
        let mut journey = opened(&text, Some(encoding)).await;
        for cursor in [start, start + 2, end] {
            let result = read(&mut journey, json!({
                "range":range(1,start,1,end),"position":position(1,cursor),"predicate":"unicode",
            })).await;
            assert_eq!(result["selection"], word);
            assert_eq!(result["word"], json!([word, range(1, start, 1, end)]));
        }
        if let Some(split) = split {
            let result = read(&mut journey, json!({
                "range":range(1,split,1,split),"position":position(1,split),"predicate":"unicode",
            })).await;
            assert_eq!(result["selection"], text);
            assert_eq!(result["word"], Value::Null);
        }
        journey.finish().await.unwrap();
    }
}
