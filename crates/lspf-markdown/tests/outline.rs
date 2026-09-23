//! Document symbols, workspace symbols, folding, selection, and the
//! capabilities that advertise them.

mod support;

use lspf::testing::ServerJourney;
use lspf::types::{
    DocumentSymbolParams, DocumentSymbolRequest, DocumentSymbolResponse, FoldingRangeKind,
    FoldingRangeParams, FoldingRangeRequest, Position, SelectionRangeParams, SelectionRangeRequest,
};
use lspf_markdown::MemoryFs;
use serde_json::json;
use support::{call, call_raw, document, open, start, uri, workspace_params};

const GUIDE: &str = "# Guide

Intro text.

## Install

- step one
- step two
  continues

```rust
fn main() {}
```

## Usage

<!-- #region demo -->

Text

<!-- #endregion -->
";

#[tokio::test]
async fn capabilities_advertise_every_language_feature() {
    let fs = MemoryFs::new();
    let server = lspf_markdown::server(fs);
    let journey = ServerJourney::start_with(server, workspace_params(json!({})))
        .await
        .unwrap();
    let initialize = journey
        .capture()
        .snapshot()
        .into_iter()
        .find_map(|event| match event.message() {
            lspf::RawMessage::Response {
                result: Ok(bytes), ..
            } => Some(serde_json::from_slice::<serde_json::Value>(bytes).unwrap()),
            _ => None,
        })
        .expect("the initialize response was captured");
    let capabilities = &initialize["capabilities"];
    for provider in [
        "hoverProvider",
        "definitionProvider",
        "documentSymbolProvider",
        "workspaceSymbolProvider",
        "foldingRangeProvider",
        "selectionRangeProvider",
        "documentLinkProvider",
        "referencesProvider",
        "documentHighlightProvider",
        "renameProvider",
        "completionProvider",
        "codeActionProvider",
    ] {
        assert!(
            !capabilities[provider].is_null(),
            "{provider} is advertised: {capabilities}"
        );
    }
    assert_eq!(capabilities["textDocumentSync"], 2);
    assert_eq!(capabilities["renameProvider"]["prepareProvider"], true);
    assert_eq!(
        capabilities["documentLinkProvider"]["resolveProvider"],
        true
    );
    assert!(
        capabilities["workspace"]["fileOperations"]["willRename"].is_object(),
        "{capabilities}"
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn document_symbols_nest_headings_by_level() {
    let fs = MemoryFs::new();
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/guide.md");
    open(&mut journey, &readme, GUIDE).await;

    let symbols = call::<DocumentSymbolRequest>(
        &mut journey,
        10,
        DocumentSymbolParams {
            text_document: document(&readme),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    )
    .await;
    let Some(DocumentSymbolResponse::DocumentSymbolList(symbols)) = symbols else {
        panic!("expected nested document symbols, got {symbols:?}");
    };
    assert_eq!(symbols.len(), 1);
    let guide = &symbols[0];
    assert_eq!(guide.name, "# Guide");
    assert_eq!(guide.range.start, Position::new(0, 0));
    assert_eq!(guide.range.end, Position::new(20, 19));
    assert_eq!(guide.selection_range.end, Position::new(0, 7));
    let children: Vec<_> = guide
        .children
        .as_ref()
        .unwrap()
        .iter()
        .map(|child| {
            (
                child.name.as_str(),
                child.range.start.line,
                child.range.end.line,
            )
        })
        .collect();
    assert_eq!(children, [("## Install", 4, 12), ("## Usage", 14, 20)]);
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn workspace_symbols_cover_files_and_skip_excluded_folders() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/a.md"), "# Alpha\n\n## Shared setup\n");
    fs.insert(uri("file:///w/sub/b.markdown"), "# Beta\n");
    fs.insert(uri("file:///w/notes.txt"), "# Not markdown\n");
    fs.insert(uri("file:///w/node_modules/pkg/x.md"), "# Hidden\n");
    fs.insert(uri("file:///w/.git/y.md"), "# Hidden too\n");
    let mut journey = start(&fs).await;
    let draft = uri("untitled:draft");
    open(&mut journey, &draft, "# Draft\n").await;

    // The response enum is untagged, so read the wire JSON directly.
    let names = |response: serde_json::Value| -> Vec<(String, String)> {
        let mut names: Vec<_> = response
            .as_array()
            .expect("a symbol list")
            .iter()
            .map(|symbol| {
                (
                    symbol["name"].as_str().unwrap().to_string(),
                    symbol["location"]["uri"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        names.sort();
        names
    };
    let query = |query: &str| json!({ "query": query });

    let all = names(
        call_raw(&mut journey, 10, "workspace/symbol", query(""))
            .await
            .unwrap(),
    );
    assert_eq!(
        all,
        [
            ("# Alpha".to_string(), "file:///w/a.md".to_string()),
            ("# Beta".to_string(), "file:///w/sub/b.markdown".to_string()),
            ("# Draft".to_string(), "untitled:draft".to_string()),
            ("## Shared setup".to_string(), "file:///w/a.md".to_string()),
        ]
    );
    let filtered = names(
        call_raw(&mut journey, 11, "workspace/symbol", query("shs"))
            .await
            .unwrap(),
    );
    assert_eq!(
        filtered,
        [("## Shared setup".to_string(), "file:///w/a.md".to_string())]
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn folding_covers_sections_regions_and_blocks() {
    let fs = MemoryFs::new();
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/guide.md");
    open(&mut journey, &readme, GUIDE).await;

    let ranges = call::<FoldingRangeRequest>(
        &mut journey,
        10,
        FoldingRangeParams {
            text_document: document(&readme),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    )
    .await
    .unwrap();
    let folds: Vec<_> = ranges
        .iter()
        .map(|range| (range.start_line, range.end_line, range.kind.clone()))
        .collect();
    for expected in [
        (0, 20, None),
        (4, 12, None),
        (6, 8, None),
        (7, 8, None),
        (10, 12, None),
        (14, 20, None),
        (16, 20, Some(FoldingRangeKind::Region)),
    ] {
        assert!(folds.contains(&expected), "{expected:?} in {folds:?}");
    }
    assert_eq!(folds.len(), 7, "{folds:?}");
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn html_comments_fold_as_comments() {
    let fs = MemoryFs::new();
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    open(
        &mut journey,
        &readme,
        "<!--\nlong\ncomment\n-->\n\n> quoted\n> text\n",
    )
    .await;
    let ranges = call::<FoldingRangeRequest>(
        &mut journey,
        10,
        FoldingRangeParams {
            text_document: document(&readme),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    )
    .await
    .unwrap();
    let folds: Vec<_> = ranges
        .iter()
        .map(|range| (range.start_line, range.end_line, range.kind.clone()))
        .collect();
    assert_eq!(
        folds,
        [(0, 3, Some(FoldingRangeKind::Comment)), (5, 6, None)]
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn selection_expands_from_inline_spans_to_the_document() {
    let fs = MemoryFs::new();
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let text = "# Top\n\n## Part\n\n- see [the **guide**](guide.md)\n- other\n";
    open(&mut journey, &readme, text).await;

    let ranges = call::<SelectionRangeRequest>(
        &mut journey,
        10,
        SelectionRangeParams {
            text_document: document(&readme),
            positions: vec![Position::new(4, 16), Position::new(40, 0)],
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    )
    .await
    .unwrap();
    assert_eq!(ranges.len(), 2);
    let mut chain = Vec::new();
    let mut current = Some(&ranges[0]);
    while let Some(selection) = current {
        chain.push((
            selection.range.start.line,
            selection.range.start.character,
            selection.range.end.line,
            selection.range.end.character,
        ));
        current = selection.parent.as_deref();
    }
    assert_eq!(
        chain,
        [
            (4, 11, 4, 20),
            (4, 6, 4, 31),
            (4, 0, 4, 31),
            (4, 0, 5, 7),
            (2, 0, 5, 7),
            (0, 0, 5, 7),
        ]
    );
    assert_eq!(ranges[1].range.start, Position::new(40, 0));
    assert!(ranges[1].parent.is_none());
    journey.finish().await.unwrap();
}
