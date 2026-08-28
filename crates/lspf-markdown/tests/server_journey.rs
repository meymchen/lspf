use std::borrow::Cow;
use std::str::FromStr;
use std::time::Duration;

use bytes::Bytes;
use lspf::testing::ServerJourney;
use lspf::types::{
    Diagnostic, DidChangeTextDocumentParams, DidOpenTextDocumentParams, GotoDefinitionParams,
    HoverParams, PartialResultParams, Position, PublishDiagnosticsParams,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, VersionedTextDocumentIdentifier, WorkDoneProgressParams,
};
use lspf::{MemoryFileProvider, RawMessage, RequestId};

fn notification(method: &'static str, params: &impl serde::Serialize) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(params).expect("notification params serialize")),
    }
}

fn request(id: i32, method: &'static str, params: &impl serde::Serialize) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(params).expect("request params serialize")),
    }
}

async fn diagnostics(journey: &mut ServerJourney) -> PublishDiagnosticsParams {
    let message = tokio::time::timeout(Duration::from_secs(1), journey.peer().recv())
        .await
        .expect("the server publishes diagnostics")
        .expect("the testing Transport stays open");
    let RawMessage::Notification { method, params } = message else {
        panic!("expected diagnostics notification, got {message:?}");
    };
    assert_eq!(method, "textDocument/publishDiagnostics");
    serde_json::from_slice(&params).expect("diagnostics params decode")
}

#[tokio::test]
async fn incremental_edits_recompute_broken_local_link_diagnostics() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let guide = lspf::types::Uri::from_str("file:///workspace/guide.md").unwrap();
    let provider = MemoryFileProvider::new();
    provider.insert(guide, "# Guide\n");
    let mut journey = ServerJourney::start(lspf_markdown::server(provider))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "markdown".into(),
                    version: 1,
                    text: "[missing](missing.md)\n".into(),
                },
            },
        ))
        .unwrap();

    let published = diagnostics(&mut journey).await;
    assert_eq!(published.uri, uri);
    assert_eq!(published.version, Some(1));
    assert_eq!(published.diagnostics.len(), 1);
    assert_eq!(
        published.diagnostics[0],
        Diagnostic {
            range: lspf::types::Range::new(
                lspf::types::Position::new(0, 10),
                lspf::types::Position::new(0, 20),
            ),
            severity: Some(lspf::types::DiagnosticSeverity::ERROR),
            source: Some("lspf-markdown".into()),
            message: "local link target does not exist: missing.md".into(),
            ..Diagnostic::default()
        }
    );

    journey
        .peer()
        .send(notification(
            "textDocument/didChange",
            &DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: Some(lspf::types::Range::new(
                        lspf::types::Position::new(0, 10),
                        lspf::types::Position::new(0, 20),
                    )),
                    range_length: None,
                    text: "guide.md".into(),
                }],
            },
        ))
        .unwrap();

    let published = diagnostics(&mut journey).await;
    assert_eq!(published.uri, uri);
    assert_eq!(published.version, Some(2));
    assert!(published.diagnostics.is_empty());

    journey.finish().await.unwrap();
}

#[tokio::test]
async fn hover_describes_the_resolved_local_target() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let guide = lspf::types::Uri::from_str("file:///workspace/guide.md").unwrap();
    let provider = MemoryFileProvider::new();
    provider.insert(guide, "# Guide\n\nWelcome.\n");
    let mut journey = ServerJourney::start(lspf_markdown::server(provider))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "markdown".into(),
                    version: 1,
                    text: "Read the [guide](guide.md).\n".into(),
                },
            },
        ))
        .unwrap();
    assert!(diagnostics(&mut journey).await.diagnostics.is_empty());

    journey
        .peer()
        .send(request(
            10,
            "textDocument/hover",
            &HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(0, 20),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        ))
        .unwrap();

    let response = journey.peer().recv().await.unwrap();
    let RawMessage::Response {
        id,
        result: Ok(result),
    } = response
    else {
        panic!("expected successful hover response, got {response:?}");
    };
    assert_eq!(id, RequestId::Number(10));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result).unwrap(),
        serde_json::json!({
            "contents": {
                "kind": "markdown",
                "value": "**Guide**\n\n`file:///workspace/guide.md`"
            },
            "range": {
                "start": { "line": 0, "character": 17 },
                "end": { "line": 0, "character": 25 }
            }
        })
    );

    journey.finish().await.unwrap();
}

#[tokio::test]
async fn definition_navigates_to_the_local_targets_first_heading() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let guide = lspf::types::Uri::from_str("file:///workspace/guide.md").unwrap();
    let provider = MemoryFileProvider::new();
    provider.insert(guide, "# Guide\n\nWelcome.\n");
    let mut journey = ServerJourney::start(lspf_markdown::server(provider))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "markdown".into(),
                    version: 1,
                    text: "Read the [guide](guide.md).\n".into(),
                },
            },
        ))
        .unwrap();
    assert!(diagnostics(&mut journey).await.diagnostics.is_empty());

    journey
        .peer()
        .send(request(
            11,
            "textDocument/definition",
            &GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position::new(0, 20),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
        ))
        .unwrap();

    let response = journey.peer().recv().await.unwrap();
    let RawMessage::Response {
        id,
        result: Ok(result),
    } = response
    else {
        panic!("expected successful definition response, got {response:?}");
    };
    assert_eq!(id, RequestId::Number(11));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result).unwrap(),
        serde_json::json!({
            "uri": "file:///workspace/guide.md",
            "range": {
                "start": { "line": 0, "character": 2 },
                "end": { "line": 0, "character": 7 }
            }
        })
    );

    journey.finish().await.unwrap();
}

#[tokio::test]
async fn diagnostics_ignore_code_examples_and_external_links() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let mut journey = ServerJourney::start(lspf_markdown::server(MemoryFileProvider::new()))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri,
                    language_id: "markdown".into(),
                    version: 1,
                    text: "`[example](missing.md)` and [website](https://example.com)\n".into(),
                },
            },
        ))
        .unwrap();

    assert!(diagnostics(&mut journey).await.diagnostics.is_empty());
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn balanced_parentheses_are_part_of_an_inline_destination() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let target = lspf::types::Uri::from_str("file:///workspace/guide_(v2).md").unwrap();
    let provider = MemoryFileProvider::new();
    provider.insert(target, "# Guide v2\n");
    let mut journey = ServerJourney::start(lspf_markdown::server(provider))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri,
                    language_id: "markdown".into(),
                    version: 1,
                    text: "Read [version two](guide_(v2).md).\n".into(),
                },
            },
        ))
        .unwrap();

    assert!(diagnostics(&mut journey).await.diagnostics.is_empty());
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn reference_links_resolve_their_local_definition_target() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let guide = lspf::types::Uri::from_str("file:///workspace/guide.md").unwrap();
    let provider = MemoryFileProvider::new();
    provider.insert(guide, "# Guide\n");
    let mut journey = ServerJourney::start(lspf_markdown::server(provider))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "markdown".into(),
                    version: 1,
                    text: "Read [the guide][docs].\n\n[docs]: guide.md\n".into(),
                },
            },
        ))
        .unwrap();
    assert!(diagnostics(&mut journey).await.diagnostics.is_empty());

    journey
        .peer()
        .send(request(
            20,
            "textDocument/hover",
            &HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(0, 19),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        ))
        .unwrap();
    let response = journey.peer().recv().await.unwrap();
    let RawMessage::Response {
        id,
        result: Ok(result),
    } = response
    else {
        panic!("expected successful hover response, got {response:?}");
    };
    assert_eq!(id, RequestId::Number(20));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result).unwrap(),
        serde_json::json!({
            "contents": {
                "kind": "markdown",
                "value": "**Guide**\n\n`file:///workspace/guide.md`"
            },
            "range": {
                "start": { "line": 0, "character": 17 },
                "end": { "line": 0, "character": 21 }
            }
        })
    );

    journey
        .peer()
        .send(request(
            21,
            "textDocument/definition",
            &GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position::new(0, 19),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
        ))
        .unwrap();
    let response = journey.peer().recv().await.unwrap();
    let RawMessage::Response {
        id,
        result: Ok(result),
    } = response
    else {
        panic!("expected successful definition response, got {response:?}");
    };
    assert_eq!(id, RequestId::Number(21));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result).unwrap(),
        serde_json::json!({
            "uri": "file:///workspace/guide.md",
            "range": {
                "start": { "line": 0, "character": 2 },
                "end": { "line": 0, "character": 7 }
            }
        })
    );

    journey.finish().await.unwrap();
}

#[tokio::test]
async fn fragment_definition_navigates_to_the_named_heading() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let guide = lspf::types::Uri::from_str("file:///workspace/guide.md").unwrap();
    let provider = MemoryFileProvider::new();
    provider.insert(guide, "# Guide\n\n## Install\n");
    let mut journey = ServerJourney::start(lspf_markdown::server(provider))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "markdown".into(),
                    version: 1,
                    text: "See [installation](guide.md#install).\n".into(),
                },
            },
        ))
        .unwrap();
    assert!(diagnostics(&mut journey).await.diagnostics.is_empty());

    journey
        .peer()
        .send(request(
            30,
            "textDocument/definition",
            &GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position::new(0, 25),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
        ))
        .unwrap();
    let response = journey.peer().recv().await.unwrap();
    let RawMessage::Response {
        id,
        result: Ok(result),
    } = response
    else {
        panic!("expected successful definition response, got {response:?}");
    };
    assert_eq!(id, RequestId::Number(30));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result).unwrap(),
        serde_json::json!({
            "uri": "file:///workspace/guide.md",
            "range": {
                "start": { "line": 2, "character": 3 },
                "end": { "line": 2, "character": 10 }
            }
        })
    );

    journey.finish().await.unwrap();
}

#[tokio::test]
async fn shortcut_reference_links_report_broken_definition_targets() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let mut journey = ServerJourney::start(lspf_markdown::server(MemoryFileProvider::new()))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri,
                    language_id: "markdown".into(),
                    version: 1,
                    text: "Read [guide].\n\n[guide]: missing.md\n".into(),
                },
            },
        ))
        .unwrap();

    let published = diagnostics(&mut journey).await;
    assert_eq!(published.diagnostics.len(), 1);
    assert_eq!(
        published.diagnostics[0].range,
        lspf::types::Range::new(Position::new(0, 6), Position::new(0, 11))
    );
    assert_eq!(
        published.diagnostics[0].message,
        "local link target does not exist: missing.md"
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn fragment_definition_uses_setext_headings_and_ignores_fenced_examples() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let guide = lspf::types::Uri::from_str("file:///workspace/guide.md").unwrap();
    let provider = MemoryFileProvider::new();
    provider.insert(guide, "```md\n# Install\n```\n\nInstall\n=======\n");
    let mut journey = ServerJourney::start(lspf_markdown::server(provider))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "markdown".into(),
                    version: 1,
                    text: "See [installation](guide.md#install).\n".into(),
                },
            },
        ))
        .unwrap();
    assert!(diagnostics(&mut journey).await.diagnostics.is_empty());

    journey
        .peer()
        .send(request(
            31,
            "textDocument/definition",
            &GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position::new(0, 25),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
        ))
        .unwrap();
    let response = journey.peer().recv().await.unwrap();
    let RawMessage::Response {
        result: Ok(result), ..
    } = response
    else {
        panic!("expected successful definition response, got {response:?}");
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result).unwrap(),
        serde_json::json!({
            "uri": "file:///workspace/guide.md",
            "range": {
                "start": { "line": 4, "character": 0 },
                "end": { "line": 4, "character": 7 }
            }
        })
    );

    journey.finish().await.unwrap();
}

#[tokio::test]
async fn diagnostics_ignore_links_in_indented_code_blocks() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let mut journey = ServerJourney::start(lspf_markdown::server(MemoryFileProvider::new()))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri,
                    language_id: "markdown".into(),
                    version: 1,
                    text: "    [example](missing.md)\n\n    [docs]: missing.md\n".into(),
                },
            },
        ))
        .unwrap();

    assert!(diagnostics(&mut journey).await.diagnostics.is_empty());
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn diagnostics_distinguish_tab_code_from_list_continuation_links() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let mut journey = ServerJourney::start(lspf_markdown::server(MemoryFileProvider::new()))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri,
                    language_id: "markdown".into(),
                    version: 1,
                    text: "\t[code](ignored.md)\n\n- item\n    [guide](missing.md)\n".into(),
                },
            },
        ))
        .unwrap();

    let published = diagnostics(&mut journey).await;
    assert_eq!(published.diagnostics.len(), 1);
    assert_eq!(
        published.diagnostics[0].message,
        "local link target does not exist: missing.md"
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn diagnostics_ignore_links_in_blockquoted_fences() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let mut journey = ServerJourney::start(lspf_markdown::server(MemoryFileProvider::new()))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri,
                    language_id: "markdown".into(),
                    version: 1,
                    text: "> ```md\n> [example](missing.md)\n> ```\n".into(),
                },
            },
        ))
        .unwrap();

    assert!(diagnostics(&mut journey).await.diagnostics.is_empty());
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn diagnostics_ignore_escaped_link_syntax() {
    let uri = lspf::types::Uri::from_str("file:///workspace/readme.md").unwrap();
    let mut journey = ServerJourney::start(lspf_markdown::server(MemoryFileProvider::new()))
        .await
        .unwrap();

    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri,
                    language_id: "markdown".into(),
                    version: 1,
                    text: "\\[example](missing.md)\n".into(),
                },
            },
        ))
        .unwrap();

    assert!(diagnostics(&mut journey).await.diagnostics.is_empty());
    journey.finish().await.unwrap();
}
