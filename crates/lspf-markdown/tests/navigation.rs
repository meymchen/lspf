//! Document links, definitions, hover previews, references, and highlights.

mod support;

use lspf::types::{
    DefinitionParams, DefinitionRequest, DocumentHighlightKind, DocumentHighlightParams,
    DocumentHighlightRequest, DocumentLink, DocumentLinkParams, DocumentLinkRequest,
    DocumentLinkResolveRequest, HoverParams, HoverRequest, Location, Position, Range,
    ReferenceContext, ReferenceParams, ReferencesRequest,
};
use lspf_markdown::MemoryFs;
use serde_json::json;
use support::{at, call, call_raw, document, open, start, start_with, uri};

fn range(start: (u32, u32), end: (u32, u32)) -> Range {
    Range::new(Position::new(start.0, start.1), Position::new(end.0, end.1))
}

#[tokio::test]
async fn document_links_resolve_targets_and_heading_lines() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide.md"), "# Guide\n\n## Install\n");
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let text = "[g](guide.md#install) [web](https://example.com/a) [r][ref] [x](guide)\n\n[ref]: guide.md\n";
    open(&mut journey, &readme, text).await;

    let links = call::<DocumentLinkRequest>(
        &mut journey,
        10,
        DocumentLinkParams {
            text_document: document(&readme),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    )
    .await
    .unwrap();
    let ranges: Vec<_> = links.iter().map(|link| link.range).collect();
    assert_eq!(
        ranges,
        [
            range((0, 4), (0, 20)),
            range((0, 28), (0, 49)),
            range((0, 55), (0, 58)),
            range((0, 64), (0, 69)),
            range((2, 7), (2, 15)),
        ]
    );
    assert_eq!(
        links[1].target.as_ref().map(|target| target.as_str()),
        Some("https://example.com/a")
    );

    let mut resolved = Vec::new();
    for (index, link) in links.into_iter().enumerate() {
        let id = 20 + i32::try_from(index).unwrap();
        let link = call::<DocumentLinkResolveRequest>(&mut journey, id, link).await;
        resolved.push(link.target.map(|target| target.as_str().to_string()));
    }
    assert_eq!(
        resolved,
        [
            Some("file:///w/guide.md#L3,1".to_string()),
            Some("https://example.com/a".to_string()),
            Some("file:///w/guide.md".to_string()),
            Some("file:///w/guide.md".to_string()),
            Some("file:///w/guide.md".to_string()),
        ]
    );

    let untouched = call::<DocumentLinkResolveRequest>(
        &mut journey,
        30,
        DocumentLink {
            range: range((0, 0), (0, 1)),
            data: Some(json!({ "unexpected": true })),
            ..DocumentLink::default()
        },
    )
    .await;
    assert!(untouched.target.is_none());
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn definitions_follow_external_references_to_their_definition() {
    let fs = MemoryFs::new();
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let text = "See [site][web] and [top](#top).\n\n# Top\n\n[web]: https://example.com\n";
    open(&mut journey, &readme, text).await;

    let definition = |line, character| DefinitionParams {
        text_document_position_params: at(&readme, line, character),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let reference = call::<DefinitionRequest>(&mut journey, 10, definition(0, 12)).await;
    assert_eq!(
        serde_json::to_value(reference).unwrap(),
        json!({
            "uri": "file:///w/readme.md",
            "range": { "start": { "line": 4, "character": 1 }, "end": { "line": 4, "character": 4 } }
        })
    );
    let fragment = call::<DefinitionRequest>(&mut journey, 11, definition(0, 27)).await;
    assert_eq!(
        serde_json::to_value(fragment).unwrap(),
        json!({
            "uri": "file:///w/readme.md",
            "range": { "start": { "line": 2, "character": 2 }, "end": { "line": 2, "character": 5 } }
        })
    );
    let plain_text = call::<DefinitionRequest>(&mut journey, 12, definition(0, 1)).await;
    assert!(plain_text.is_none());
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn hover_previews_existing_images_only() {
    let fs = MemoryFs::new();
    fs.insert_binary(uri("file:///w/img/logo.png"));
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let published = open(
        &mut journey,
        &readme,
        "![logo](img/logo.png) ![gone](img/gone.png) [web](https://example.com)\n",
    )
    .await;
    assert_eq!(
        published.diagnostics.len(),
        1,
        "only the missing image is reported"
    );

    let hover = |character| HoverParams {
        text_document_position_params: at(&readme, 0, character),
        work_done_progress_params: Default::default(),
    };
    let preview = call::<HoverRequest>(&mut journey, 10, hover(10))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&preview.contents).unwrap()["value"],
        "![](file:///w/img/logo.png)"
    );
    assert_eq!(preview.range, Some(range((0, 8), (0, 20))));
    assert!(
        call::<HoverRequest>(&mut journey, 11, hover(32))
            .await
            .is_none()
    );
    assert!(
        call::<HoverRequest>(&mut journey, 12, hover(55))
            .await
            .is_none()
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn positions_follow_the_negotiated_utf16_encoding() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide.md"), "# 指南 🚀\n");
    let mut journey = start_with(
        &fs,
        json!({ "general": { "positionEncodings": ["utf-16"] } }),
    )
    .await;
    let readme = uri("file:///w/readme.md");
    let published = open(&mut journey, &readme, "文档 🚀 [链接](missing.md)\n").await;
    // 🚀 is two UTF-16 code units, so `missing.md` starts at unit 11.
    assert_eq!(published.diagnostics[0].range, range((0, 11), (0, 21)));

    let definition = call::<DefinitionRequest>(
        &mut journey,
        10,
        DefinitionParams {
            text_document_position_params: at(&readme, 0, 12),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    )
    .await;
    assert!(definition.is_none(), "missing targets have no definition");
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn references_find_headings_files_and_labels_across_the_workspace() {
    let fs = MemoryFs::new();
    let guide_text = "# Guide\n\n## Install\n\nSee [below](#install).\n";
    fs.insert(uri("file:///w/guide.md"), guide_text);
    fs.insert(
        uri("file:///w/docs/other.md"),
        "[a](../guide.md#install) [b](/guide.md) [c](../guide)\n",
    );
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let text = "[i](guide.md#Install) [g](guide.md) [r][ref] [r2][REF]\n\n[ref]: guide.md\n";
    open(&mut journey, &readme, text).await;

    let references = |line, character, include_declaration| ReferenceParams {
        context: ReferenceContext {
            include_declaration,
        },
        text_document_position_params: at(&readme, line, character),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let summarize = |locations: Vec<Location>| -> Vec<(String, u32, u32, u32)> {
        locations
            .into_iter()
            .map(|location| {
                (
                    location
                        .uri
                        .as_str()
                        .trim_start_matches("file:///w/")
                        .to_string(),
                    location.range.start.line,
                    location.range.start.character,
                    location.range.end.character,
                )
            })
            .collect()
    };

    let heading = call::<ReferencesRequest>(&mut journey, 10, references(0, 15, true))
        .await
        .unwrap();
    assert_eq!(
        summarize(heading),
        [
            ("docs/other.md".to_string(), 0, 16, 23),
            ("guide.md".to_string(), 2, 3, 10),
            ("guide.md".to_string(), 4, 13, 20),
            ("readme.md".to_string(), 0, 13, 20),
        ]
    );

    let file = call::<ReferencesRequest>(&mut journey, 11, references(0, 28, false))
        .await
        .unwrap();
    assert_eq!(
        summarize(file),
        [
            ("docs/other.md".to_string(), 0, 4, 15),
            ("docs/other.md".to_string(), 0, 29, 38),
            ("docs/other.md".to_string(), 0, 44, 52),
            ("readme.md".to_string(), 0, 4, 12),
            ("readme.md".to_string(), 0, 26, 34),
            ("readme.md".to_string(), 2, 7, 15),
        ]
    );

    let label_uses = call::<ReferencesRequest>(&mut journey, 12, references(0, 41, false))
        .await
        .unwrap();
    assert_eq!(
        summarize(label_uses),
        [
            ("readme.md".to_string(), 0, 40, 43),
            ("readme.md".to_string(), 0, 50, 53),
        ]
    );
    let with_definition = call::<ReferencesRequest>(&mut journey, 13, references(2, 2, true))
        .await
        .unwrap();
    assert_eq!(with_definition.len(), 3);

    let external = call_raw(
        &mut journey,
        14,
        "textDocument/references",
        json!({
            "textDocument": { "uri": "file:///w/readme.md" },
            "position": { "line": 1, "character": 0 },
            "context": { "includeDeclaration": true }
        }),
    )
    .await
    .unwrap();
    assert_eq!(external, serde_json::Value::Null);
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn highlights_mark_the_declaration_as_a_write() {
    let fs = MemoryFs::new();
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let text = "# Top\n\n[up](#top) [again](#TOP) [other](other.md#top)\n";
    open(&mut journey, &readme, text).await;

    let highlights = call::<DocumentHighlightRequest>(
        &mut journey,
        10,
        DocumentHighlightParams {
            text_document_position_params: at(&readme, 0, 3),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    )
    .await
    .unwrap();
    let marks: Vec<_> = highlights
        .into_iter()
        .map(|highlight| (highlight.range, highlight.kind))
        .collect();
    assert_eq!(
        marks,
        [
            (range((0, 2), (0, 5)), Some(DocumentHighlightKind::Write)),
            (range((2, 6), (2, 9)), Some(DocumentHighlightKind::Read)),
            (range((2, 20), (2, 23)), Some(DocumentHighlightKind::Read)),
        ]
    );
    journey.finish().await.unwrap();
}
