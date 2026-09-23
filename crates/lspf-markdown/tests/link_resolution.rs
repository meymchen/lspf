//! Shared target identity across navigation, references, and edits.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use lspf::testing::ServerJourney;
use lspf::types::Uri;
use lspf::{FileProvider, WorkspaceError};
use lspf_markdown::{FileKind, MemoryFs, WorkspaceFs};
use serde_json::{Value, json};
use support::{call_raw, open, start, start_with, uri, workspace_params};

fn position(line: u32, character: u32) -> Value {
    json!({
        "textDocument": { "uri": "file:///w/readme.md" },
        "position": { "line": line, "character": character }
    })
}

#[tokio::test]
async fn missing_targets_keep_exact_references_but_cannot_be_renamed() {
    let fs = MemoryFs::new();
    let mut journey = start_with(&fs, json!({
        "workspace": { "workspaceEdit": { "documentChanges": true, "resourceOperations": ["rename"] } }
    })).await;
    let published = open(
        &mut journey,
        &uri("file:///w/readme.md"),
        "[missing](guide)\n[md](guide.md)\n",
    )
    .await;
    assert_eq!(published.diagnostics.len(), 2);
    let mut references = position(0, 11);
    references["context"] = json!({ "includeDeclaration": false });
    let references = call_raw(&mut journey, 10, "textDocument/references", references)
        .await
        .unwrap();
    assert_eq!(
        references,
        json!([{
            "uri": "file:///w/readme.md",
            "range": { "start": { "line": 0, "character": 10 }, "end": { "line": 0, "character": 15 } }
        }])
    );
    let prepared = call_raw(
        &mut journey,
        11,
        "textDocument/prepareRename",
        position(0, 11),
    )
    .await;
    let mut rename = position(0, 11);
    rename["newName"] = json!("manual.md");
    let renamed = call_raw(&mut journey, 12, "textDocument/rename", rename).await;
    assert!(
        prepared.is_err(),
        "a missing resource has no rename selection"
    );
    assert!(
        renamed.is_err(),
        "a missing resource must not produce a file move"
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn moving_a_markdown_file_does_not_move_its_extensionless_neighbor() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide.md"), "# Markdown\n");
    let mut journey = start(&fs).await;
    open(&mut journey, &uri("file:///w/guide"), "[home](readme.md)\n").await;
    open(
        &mut journey,
        &uri("file:///w/readme.md"),
        "[plain](guide)\n[md](guide.md)\n",
    )
    .await;
    let edit = call_raw(
        &mut journey,
        10,
        "workspace/willRenameFiles",
        json!({
            "files": [{ "oldUri": "file:///w/guide.md", "newUri": "file:///w/docs/manual.md" }]
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        edit["changes"],
        json!({
            "file:///w/readme.md": [{
                "range": { "start": { "line": 1, "character": 5 }, "end": { "line": 1, "character": 13 } },
                "newText": "docs/manual.md"
            }]
        })
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn rewritten_paths_still_select_the_moved_resource() {
    for (new_uri, collision, expected) in [
        ("file:///w/manual.markdown", false, "manual.markdown"),
        ("file:///w/manual.md", true, "manual.md"),
        ("file:///w/manual.md", false, "manual"),
    ] {
        let fs = MemoryFs::new();
        fs.insert(uri("file:///w/guide.md"), "# Guide\n");
        if collision {
            fs.insert(uri("file:///w/manual"), "# Another resource\n");
        }
        let mut journey = start(&fs).await;
        open(&mut journey, &uri("file:///w/readme.md"), "[g](guide)\n").await;
        let edit = call_raw(
            &mut journey,
            10,
            "workspace/willRenameFiles",
            json!({
                "files": [{ "oldUri": "file:///w/guide.md", "newUri": new_uri }]
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            edit["changes"]["file:///w/readme.md"],
            json!([{
                "range": { "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 9 } },
                "newText": expected
            }]),
            "the rewritten link must select {new_uri}"
        );
        journey.finish().await.unwrap();
    }
}

#[tokio::test]
async fn a_move_outside_the_workspace_uses_an_unambiguous_destination() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide.md"), "# Guide\n");
    let mut journey = start(&fs).await;
    open(
        &mut journey,
        &uri("file:///w/readme.md"),
        "[g](/guide.md)\n",
    )
    .await;
    let edit = call_raw(
        &mut journey,
        10,
        "workspace/willRenameFiles",
        json!({
            "files": [{ "oldUri": "file:///w/guide.md", "newUri": "file:///outside/manual.md" }]
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        edit["changes"]["file:///w/readme.md"][0]["newText"],
        "file:///outside/manual.md"
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn an_empty_destination_does_not_offer_a_resource_rename() {
    let fs = MemoryFs::new();
    let mut journey = start(&fs).await;
    open(&mut journey, &uri("file:///w/readme.md"), "[empty]()\n").await;
    assert!(
        call_raw(
            &mut journey,
            10,
            "textDocument/prepareRename",
            position(0, 8)
        )
        .await
        .is_err()
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn highlights_keep_the_declaration_when_a_self_link_uses_an_equivalent_uri() {
    let fs = MemoryFs::new();
    let mut journey = start(&fs).await;
    open(
        &mut journey,
        &uri("file:///w/readme.md"),
        "# Title\n\n[go](readme%2Emd#title)\n",
    )
    .await;
    let highlights = call_raw(
        &mut journey,
        10,
        "textDocument/documentHighlight",
        position(2, 18),
    )
    .await
    .unwrap();
    assert_eq!(
        highlights,
        json!([
            { "range": { "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 7 } }, "kind": 3 },
            { "range": { "start": { "line": 2, "character": 17 }, "end": { "line": 2, "character": 22 } }, "kind": 2 }
        ])
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn extension_omission_uses_the_complete_projected_move_set() {
    for (moves, expected) in [
        (
            json!([
                { "oldUri": "file:///w/guide.md", "newUri": "file:///w/manual.md" },
                { "oldUri": "file:///w/manual", "newUri": "file:///w/elsewhere" }
            ]),
            "manual",
        ),
        (
            json!([
                { "oldUri": "file:///w/manual", "newUri": "file:///w/guide" }
            ]),
            "guide.md",
        ),
    ] {
        let fs = MemoryFs::new();
        fs.insert(uri("file:///w/guide.md"), "# Guide\n");
        fs.insert(uri("file:///w/manual"), "# Another resource\n");
        let mut journey = start(&fs).await;
        open(&mut journey, &uri("file:///w/readme.md"), "[g](guide)\n").await;
        let edit = call_raw(
            &mut journey,
            10,
            "workspace/willRenameFiles",
            json!({ "files": moves }),
        )
        .await
        .unwrap();
        assert_eq!(
            edit["changes"]["file:///w/readme.md"][0]["newText"],
            expected
        );
        journey.finish().await.unwrap();
    }
}

#[tokio::test]
async fn moves_preserve_link_style_and_leave_queries_and_fragments_intact() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide.md"), "# Heading\n");
    let mut journey = start(&fs).await;
    open(
        &mut journey,
        &uri("file:///w/readme.md"),
        "[a](./guide)\n[b](/guide.md)\n[c](file:///w/guide.md)\n[d](guide.md?raw#heading)\n",
    )
    .await;
    let edit = call_raw(&mut journey, 10, "workspace/willRenameFiles", json!({
        "files": [{ "oldUri": "file:///w/guide.md", "newUri": "file:///w/%E4%B8%AD%E6%96%87.md" }]
    })).await.unwrap();
    assert_eq!(
        edit["changes"]["file:///w/readme.md"],
        json!([
            { "range": { "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 11 } }, "newText": "./中文" },
            { "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 13 } }, "newText": "/中文.md" },
            { "range": { "start": { "line": 2, "character": 4 }, "end": { "line": 2, "character": 22 } }, "newText": "file:///w/%E4%B8%AD%E6%96%87.md" },
            { "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 12 } }, "newText": "中文.md" }
        ])
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn non_default_extensions_and_exact_directories_have_no_markdown_alias() {
    for (explicit, directory, selected) in [
        ("guide.markdown", false, "file:///w/guide"),
        ("guide.md", true, "file:///w/guide"),
    ] {
        let fs = MemoryFs::new();
        fs.insert(uri(&format!("file:///w/{explicit}")), "# Guide\n");
        if directory {
            fs.insert(uri("file:///w/guide/image.png"), "");
        }
        let mut journey = start(&fs).await;
        let published = open(
            &mut journey,
            &uri("file:///w/readme.md"),
            &format!("[short](guide)\n[explicit]({explicit})\n"),
        )
        .await;
        assert_eq!(published.diagnostics.len(), usize::from(!directory));
        let links = call_raw(
            &mut journey,
            10,
            "textDocument/documentLink",
            json!({ "textDocument": { "uri": "file:///w/readme.md" } }),
        )
        .await
        .unwrap();
        let resolved = call_raw(&mut journey, 11, "documentLink/resolve", links[0].clone())
            .await
            .unwrap();
        assert_eq!(resolved["target"], selected);
        let mut params = position(1, 12);
        params["context"] = json!({ "includeDeclaration": false });
        let references = call_raw(&mut journey, 12, "textDocument/references", params)
            .await
            .unwrap();
        let references = references.as_array().unwrap();
        assert_eq!(references.len(), 1);
        assert_eq!(references[0]["range"]["start"]["line"], 1);
        journey.finish().await.unwrap();
    }
}

#[tokio::test]
async fn heading_rename_only_edits_links_that_select_its_resource() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide"), "# Title\n");
    fs.insert(uri("file:///w/guide.md"), "# Title\n");
    let mut journey = start(&fs).await;
    open(
        &mut journey,
        &uri("file:///w/readme.md"),
        "[plain](guide#title)\n[md](guide.md#title)\n",
    )
    .await;
    let mut params = position(1, 15);
    params["newName"] = json!("New title");
    let edit = call_raw(&mut journey, 10, "textDocument/rename", params)
        .await
        .unwrap();
    assert_eq!(
        edit["changes"],
        json!({
            "file:///w/guide.md": [{
                "range": { "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 7 } },
                "newText": "New title"
            }],
            "file:///w/readme.md": [{
                "range": { "start": { "line": 1, "character": 14 }, "end": { "line": 1, "character": 19 } },
                "newText": "new-title"
            }]
        })
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn unsaved_exact_documents_win_over_filesystem_fallbacks() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide.md"), "# Disk heading\n");
    let mut journey = start(&fs).await;
    open(&mut journey, &uri("file:///w/guide"), "\n\n# Unsaved\n").await;
    let published = open(
        &mut journey,
        &uri("file:///w/readme.md"),
        "[g](guide#unsaved)\n",
    )
    .await;
    assert!(published.diagnostics.is_empty());
    let definition = call_raw(&mut journey, 10, "textDocument/definition", position(0, 11))
        .await
        .unwrap();
    assert_eq!(
        definition,
        json!({
            "uri": "file:///w/guide",
            "range": { "start": { "line": 2, "character": 2 }, "end": { "line": 2, "character": 9 } }
        })
    );
    journey.finish().await.unwrap();
}

#[derive(Clone, Default)]
struct CountedFs {
    files: MemoryFs,
    stats: Arc<Mutex<BTreeMap<String, usize>>>,
}

impl CountedFs {
    fn take_stats(&self) -> BTreeMap<String, usize> {
        std::mem::take(&mut *self.stats.lock().unwrap())
    }

    async fn start(&self) -> ServerJourney {
        ServerJourney::start_with(
            lspf_markdown::server(self.clone()),
            workspace_params(json!({})),
        )
        .await
        .unwrap()
    }
}

impl FileProvider for CountedFs {
    async fn read_text(&self, uri: &Uri) -> Result<Option<String>, WorkspaceError> {
        self.files.read_text(uri).await
    }
}

impl WorkspaceFs for CountedFs {
    async fn stat(&self, uri: &Uri) -> Option<FileKind> {
        *self
            .stats
            .lock()
            .unwrap()
            .entry(uri.as_str().to_string())
            .or_default() += 1;
        self.files.stat(uri).await
    }

    async fn read_directory(&self, uri: &Uri) -> Vec<(String, FileKind)> {
        self.files.read_directory(uri).await
    }
}

#[tokio::test]
async fn positive_and_negative_existence_checks_are_shared_only_within_one_invocation() {
    let fs = CountedFs::default();
    fs.files.insert(uri("file:///w/guide.md"), "# Guide\n");
    let mut journey = fs.start().await;
    open(
        &mut journey,
        &uri("file:///w/readme.md"),
        "[a](guide)\n[b](guide.md)\n[c](guide)\n[x](missing)\n[y](missing)\n",
    )
    .await;
    let expected = BTreeMap::from([
        ("file:///w/guide".to_string(), 1),
        ("file:///w/guide.md".to_string(), 1),
        ("file:///w/missing".to_string(), 1),
        ("file:///w/missing.md".to_string(), 1),
    ]);
    assert_eq!(
        fs.take_stats(),
        expected,
        "diagnostics share checks across links"
    );
    let mut params = position(0, 5);
    params["context"] = json!({ "includeDeclaration": false });
    let references = call_raw(&mut journey, 10, "textDocument/references", params.clone())
        .await
        .unwrap();
    assert_eq!(references.as_array().unwrap().len(), 3);
    assert_eq!(
        fs.take_stats(),
        expected,
        "cursor and occurrence discovery share checks"
    );

    fs.files
        .insert(uri("file:///w/guide"), "# Exact now exists\n");
    fs.files
        .insert(uri("file:///w/missing.md"), "# Missing now exists\n");
    let references = call_raw(&mut journey, 11, "textDocument/references", params)
        .await
        .unwrap();
    assert_eq!(references.as_array().unwrap().len(), 2);
    assert_eq!(
        fs.take_stats(),
        expected,
        "the next request repeats its own checks"
    );
    let definition = call_raw(&mut journey, 12, "textDocument/definition", position(3, 5))
        .await
        .unwrap();
    assert_eq!(
        definition["uri"], "file:///w/missing.md",
        "a previous negative lookup cannot hide a new resource"
    );
    assert_eq!(
        fs.take_stats(),
        BTreeMap::from([
            ("file:///w/missing".to_string(), 1),
            ("file:///w/missing.md".to_string(), 1),
        ])
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn document_link_listing_defers_existence_checks_until_resolve() {
    let fs = CountedFs::default();
    fs.files.insert(uri("file:///w/guide.md"), "# Guide\n");
    let mut journey = fs.start().await;
    open(&mut journey, &uri("file:///w/readme.md"), "[g](guide)\n").await;
    fs.take_stats();
    let links = call_raw(
        &mut journey,
        10,
        "textDocument/documentLink",
        json!({ "textDocument": { "uri": "file:///w/readme.md" } }),
    )
    .await
    .unwrap();
    assert_eq!(links.as_array().unwrap().len(), 1);
    assert!(fs.take_stats().is_empty());
    let resolved = call_raw(&mut journey, 11, "documentLink/resolve", links[0].clone())
        .await
        .unwrap();
    assert_eq!(resolved["target"], "file:///w/guide.md");
    assert_eq!(
        fs.take_stats(),
        BTreeMap::from([
            ("file:///w/guide".to_string(), 1),
            ("file:///w/guide.md".to_string(), 1),
        ])
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn navigation_and_references_agree_when_exact_and_markdown_targets_exist() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide"), "# Exact\n");
    fs.insert(uri("file:///w/guide.md"), "# Markdown\n");
    let mut journey = start(&fs).await;
    open(
        &mut journey,
        &uri("file:///w/readme.md"),
        "[plain](guide)\n[markdown](guide.md)\n",
    )
    .await;

    let definition = call_raw(&mut journey, 10, "textDocument/definition", position(0, 9))
        .await
        .unwrap();
    assert_eq!(definition["uri"], "file:///w/guide");

    let mut params = position(1, 12);
    params["context"] = json!({ "includeDeclaration": false });
    let references = call_raw(&mut journey, 11, "textDocument/references", params)
        .await
        .unwrap();
    assert_eq!(
        references,
        json!([{
            "uri": "file:///w/readme.md",
            "range": { "start": { "line": 1, "character": 11 }, "end": { "line": 1, "character": 19 } }
        }])
    );
    journey.finish().await.unwrap();
}
