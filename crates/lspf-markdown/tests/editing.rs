//! Rename, completion, code actions, file-rename link updates, and the
//! link-definition diagnostics they act on.

mod support;

use lspf::types::{
    CodeActionContext, CodeActionKind, CodeActionParams, CodeActionRequest, CodeActionResponse,
    CompletionParams, CompletionRequest, CompletionResponse, DiagnosticSeverity, DiagnosticTag,
    DidChangeWatchedFilesParams, FileChangeType, FileEvent, Position, PrepareRenameParams,
    PrepareRenameRequest, Range, RenameParams, RenameRequest, TextEdit, Uri, WorkspaceEdit,
};
use lspf_markdown::MemoryFs;
use serde_json::{Value, json};
use support::{
    at, call, call_raw, diagnostics, document, notification, open, start, start_with, uri,
};

fn range(start: (u32, u32), end: (u32, u32)) -> Range {
    Range::new(Position::new(start.0, start.1), Position::new(end.0, end.1))
}

/// Flatten a workspace edit into `(file, line, start, end, new text)` rows
/// sorted by file and position.
fn rows(edit: &Value) -> Vec<(String, u32, u32, u32, String)> {
    let mut rows = Vec::new();
    let mut push = |uri: &str, edit: &Value| {
        rows.push((
            uri.trim_start_matches("file:///w/").to_string(),
            edit["range"]["start"]["line"].as_u64().unwrap() as u32,
            edit["range"]["start"]["character"].as_u64().unwrap() as u32,
            edit["range"]["end"]["character"].as_u64().unwrap() as u32,
            edit["newText"].as_str().unwrap().to_string(),
        ));
    };
    if let Some(changes) = edit["changes"].as_object() {
        for (uri, edits) in changes {
            for edit in edits.as_array().unwrap() {
                push(uri, edit);
            }
        }
    }
    if let Some(changes) = edit["documentChanges"].as_array() {
        for change in changes {
            if let Some(edits) = change["edits"].as_array() {
                for edit in edits {
                    push(change["textDocument"]["uri"].as_str().unwrap(), edit);
                }
            }
        }
    }
    rows.sort();
    rows
}

fn row(file: &str, line: u32, start: u32, end: u32, text: &str) -> (String, u32, u32, u32, String) {
    (file.to_string(), line, start, end, text.to_string())
}

fn rename_params(uri: &Uri, line: u32, character: u32, new_name: &str) -> RenameParams {
    RenameParams {
        new_name: new_name.to_string(),
        text_document_position_params: at(uri, line, character),
        work_done_progress_params: Default::default(),
    }
}

#[tokio::test]
async fn renaming_a_heading_updates_fragments_across_the_workspace() {
    let fs = MemoryFs::new();
    fs.insert(
        uri("file:///w/docs/other.md"),
        "[a](../readme.md#install-steps) [b](../readme.md#other)\n",
    );
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let text = "# Install steps\n\nSee [up](#install-steps).\n";
    open(&mut journey, &readme, text).await;

    let prepared = call::<PrepareRenameRequest>(
        &mut journey,
        10,
        PrepareRenameParams {
            text_document_position_params: at(&readme, 0, 4),
            work_done_progress_params: Default::default(),
        },
    )
    .await;
    assert_eq!(
        serde_json::to_value(prepared).unwrap(),
        json!({
            "range": { "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 15 } },
            "placeholder": "Install steps"
        })
    );

    let edit = call::<RenameRequest>(
        &mut journey,
        11,
        rename_params(&readme, 0, 4, "Setup Guide"),
    )
    .await
    .unwrap();
    assert_eq!(
        rows(&serde_json::to_value(&edit).unwrap()),
        [
            row("docs/other.md", 0, 17, 30, "setup-guide"),
            row("readme.md", 0, 2, 15, "Setup Guide"),
            row("readme.md", 2, 10, 23, "setup-guide"),
        ]
    );

    let from_fragment = call::<PrepareRenameRequest>(
        &mut journey,
        12,
        PrepareRenameParams {
            text_document_position_params: at(&readme, 2, 14),
            work_done_progress_params: Default::default(),
        },
    )
    .await;
    assert_eq!(
        serde_json::to_value(from_fragment).unwrap()["placeholder"],
        "install-steps"
    );

    let nowhere = call_raw(
        &mut journey,
        13,
        "textDocument/prepareRename",
        json!({ "textDocument": { "uri": "file:///w/readme.md" }, "position": { "line": 2, "character": 1 } }),
    )
    .await;
    assert!(
        nowhere
            .unwrap_err()
            .contains("renaming is not supported here")
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn renaming_a_label_updates_its_definition_and_uses() {
    let fs = MemoryFs::new();
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let text = "[a][Docs] [Docs][] [docs]\n\n[docs]: guide.md\n";
    open(&mut journey, &readme, text).await;

    let edit = call::<RenameRequest>(&mut journey, 10, rename_params(&readme, 2, 2, "manual"))
        .await
        .unwrap();
    assert_eq!(
        rows(&serde_json::to_value(&edit).unwrap()),
        [
            row("readme.md", 0, 4, 8, "manual"),
            row("readme.md", 0, 11, 15, "manual"),
            row("readme.md", 0, 20, 24, "manual"),
            row("readme.md", 2, 1, 5, "manual"),
        ]
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn renaming_a_link_path_renames_the_file_and_its_links() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide.md"), "# Guide\n\n[home](readme.md)\n");
    fs.insert(
        uri("file:///w/docs/other.md"),
        "[g](../guide.md#guide) [x](/guide)\n",
    );
    let readme = uri("file:///w/readme.md");
    let text = "[g](guide.md)\n";

    let mut plain = start(&fs).await;
    open(&mut plain, &readme, text).await;
    let refused = call_raw(
        &mut plain,
        10,
        "textDocument/rename",
        json!({ "textDocument": { "uri": "file:///w/readme.md" }, "position": { "line": 0, "character": 6 }, "newName": "manual.md" }),
    )
    .await;
    assert!(refused.unwrap_err().contains("cannot rename files"));
    plain.finish().await.unwrap();

    let mut journey = start_with(
        &fs,
        json!({ "workspace": { "workspaceEdit": { "documentChanges": true, "resourceOperations": ["rename"] } } }),
    )
    .await;
    open(&mut journey, &readme, text).await;
    let edit = call::<RenameRequest>(&mut journey, 11, rename_params(&readme, 0, 6, "manual"))
        .await
        .unwrap();
    let edit = serde_json::to_value(&edit).unwrap();
    assert_eq!(
        rows(&edit),
        [
            row("docs/other.md", 0, 4, 15, "../manual.md"),
            row("docs/other.md", 0, 27, 33, "/manual"),
            row("readme.md", 0, 4, 12, "manual.md"),
        ]
    );
    let changes = edit["documentChanges"].as_array().unwrap();
    assert_eq!(
        changes.last().unwrap(),
        &json!({ "kind": "rename", "oldUri": "file:///w/guide.md", "newUri": "file:///w/manual.md" })
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn will_rename_files_rewrites_links_into_and_out_of_moved_files() {
    let fs = MemoryFs::new();
    fs.insert(
        uri("file:///w/docs/guide.md"),
        "[home](../readme.md) [img](./img/a.png)\n",
    );
    fs.insert(uri("file:///w/docs/img/a.png"), "");
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let text = "[g](docs/guide.md#top) [i](docs/img/a.png) [r](/docs/guide) [ext](https://x.dev)\n";
    open(&mut journey, &readme, text).await;

    let edit = call_raw(
        &mut journey,
        10,
        "workspace/willRenameFiles",
        json!({ "files": [{ "oldUri": "file:///w/docs", "newUri": "file:///w/manual/docs" }] }),
    )
    .await
    .unwrap();
    assert_eq!(
        rows(&edit),
        [
            row("docs/guide.md", 0, 7, 19, "../../readme.md"),
            row("readme.md", 0, 4, 17, "manual/docs/guide.md"),
            row("readme.md", 0, 27, 41, "manual/docs/img/a.png"),
            row("readme.md", 0, 47, 58, "/manual/docs/guide"),
        ]
    );

    let untouched = call_raw(
        &mut journey,
        11,
        "workspace/willRenameFiles",
        json!({ "files": [{ "oldUri": "file:///w/unrelated.md", "newUri": "file:///w/other.md" }] }),
    )
    .await
    .unwrap();
    assert_eq!(untouched, Value::Null);
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn completion_offers_paths_headings_and_labels() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide.md"), "# Guide\n\n## Install it\n");
    fs.insert(uri("file:///w/docs/my%20notes.md"), "# Notes\n");
    fs.insert(uri("file:///w/.hidden/x.md"), "# X\n");
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let text =
        "# Readme\n\n[a](\n[b](docs/\n[c](guide.md#\n[d][\n```\n[e](\n```\n\n[ref]: guide.md\n";
    open(&mut journey, &readme, text).await;

    let mut next_id = 10;
    let mut complete = |line: u32, character: u32| {
        next_id += 1;
        (
            next_id,
            CompletionParams {
                context: None,
                text_document_position_params: at(&readme, line, character),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
        )
    };
    let labels = |response: Option<CompletionResponse>| -> Vec<(String, String)> {
        let Some(CompletionResponse::CompletionItemList(items)) = response else {
            panic!("expected completion items, got {response:?}");
        };
        items
            .into_iter()
            .map(|item| {
                let Some(lspf::types::CompletionItemTextEdit::TextEdit(edit)) = item.text_edit
                else {
                    panic!("expected a text edit");
                };
                (item.label, edit.new_text)
            })
            .collect()
    };

    let (id, params) = complete(2, 4);
    let root = labels(call::<CompletionRequest>(&mut journey, id, params).await);
    assert_eq!(
        root,
        [
            ("docs/".to_string(), "docs/".to_string()),
            ("guide.md".to_string(), "guide.md".to_string()),
            ("#readme".to_string(), "#readme".to_string()),
        ]
    );
    let (id, params) = complete(3, 9);
    let docs = labels(call::<CompletionRequest>(&mut journey, id, params).await);
    assert_eq!(
        docs,
        [("my notes.md".to_string(), "my%20notes.md".to_string())]
    );
    let (id, params) = complete(4, 13);
    let headings = labels(call::<CompletionRequest>(&mut journey, id, params).await);
    assert_eq!(
        headings,
        [
            ("#guide".to_string(), "guide".to_string()),
            ("#install-it".to_string(), "install-it".to_string()),
        ]
    );
    let (id, params) = complete(5, 4);
    let references = labels(call::<CompletionRequest>(&mut journey, id, params).await);
    assert_eq!(references, [("ref".to_string(), "ref".to_string())]);
    let (id, params) = complete(7, 4);
    assert!(
        call::<CompletionRequest>(&mut journey, id, params)
            .await
            .is_none()
    );
    let (id, params) = complete(0, 3);
    assert!(
        call::<CompletionRequest>(&mut journey, id, params)
            .await
            .is_none()
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn definition_diagnostics_report_broken_duplicate_and_unused_labels() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide.md"), "# Guide\n");
    fs.insert_binary(uri("file:///w/logo.png"));
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let text = "[a][used] [b][missing] ![l](logo.png) [d](docs) [g](guide) [h](#nowhere) [n](guide.md#L3)\n\n[used]: guide.md\n[used]: other.md\n[spare]: guide.md\n";
    fs.insert(uri("file:///w/docs/index.md"), "# Docs\n");
    let published = open(&mut journey, &readme, text).await;
    let summary: Vec<_> = published
        .diagnostics
        .iter()
        .map(|diagnostic| {
            (
                diagnostic.range.start.line,
                diagnostic.range.start.character,
                diagnostic.severity,
                serde_json::to_value(&diagnostic.code).unwrap(),
                serde_json::to_value(&diagnostic.message).unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                0,
                14,
                Some(DiagnosticSeverity::Error),
                json!("link.no-such-reference"),
                json!("no link definition found: 'missing'"),
            ),
            (
                0,
                63,
                Some(DiagnosticSeverity::Error),
                Value::Null,
                json!("local link heading does not exist: #nowhere"),
            ),
            (
                3,
                1,
                Some(DiagnosticSeverity::Warning),
                json!("link.duplicate-definition"),
                json!("link definition for 'used' already exists"),
            ),
            (
                4,
                0,
                Some(DiagnosticSeverity::Hint),
                json!("link.unused-definition"),
                json!("link definition is unused"),
            ),
        ]
    );
    assert_eq!(
        published.diagnostics[3].tags,
        Some(vec![DiagnosticTag::Unnecessary])
    );

    let actions = call::<CodeActionRequest>(
        &mut journey,
        10,
        CodeActionParams {
            text_document: document(&readme),
            range: range((3, 1), (3, 1)),
            context: CodeActionContext {
                diagnostics: published.diagnostics[2..].to_vec(),
                only: Some(vec![CodeActionKind::QuickFix]),
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    )
    .await
    .unwrap();
    let fixes: Vec<_> = actions
        .iter()
        .map(|action| {
            let CodeActionResponse::CodeAction(action) = action else {
                panic!("expected a code action");
            };
            let edit = action.edit.as_ref().unwrap();
            let edits = &edit.changes.as_ref().unwrap()[&readme];
            (action.title.clone(), edits[0].range)
        })
        .collect();
    assert_eq!(
        fixes,
        [
            (
                "Remove duplicate link definition".to_string(),
                range((3, 0), (4, 0))
            ),
            (
                "Remove unused link definition".to_string(),
                range((4, 0), (5, 0))
            ),
        ]
    );
    journey.finish().await.unwrap();
}

fn only_edit(action: &CodeActionResponse, uri: &Uri) -> (String, Vec<TextEdit>) {
    let CodeActionResponse::CodeAction(action) = action else {
        panic!("expected a code action");
    };
    let edit: &WorkspaceEdit = action.edit.as_ref().unwrap();
    (
        action.title.clone(),
        edit.changes.as_ref().unwrap()[uri].clone(),
    )
}

#[tokio::test]
async fn organize_and_extract_rewrite_link_definitions() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide.md"), "# Guide\n");
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let text = "[z]: guide.md\n\nUse [b][b] and [z].\n\n[b]: guide.md\n[unused]: guide.md\n\nMore [g](guide.md) and [again](guide.md).";
    open(&mut journey, &readme, text).await;

    let organize = call::<CodeActionRequest>(
        &mut journey,
        10,
        CodeActionParams {
            text_document: document(&readme),
            range: range((0, 0), (0, 0)),
            context: CodeActionContext {
                diagnostics: Vec::new(),
                only: Some(vec![CodeActionKind::Source]),
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    )
    .await
    .unwrap();
    let organized: Vec<_> = organize
        .iter()
        .map(|action| only_edit(action, &readme))
        .map(|(title, edits)| (title, edits[0].new_text.clone()))
        .collect();
    assert_eq!(
        organized,
        [
            (
                "Organize link definitions".to_string(),
                "Use [b][b] and [z].\n\nMore [g](guide.md) and [again](guide.md).\n\n[b]: guide.md\n[unused]: guide.md\n[z]: guide.md\n".to_string(),
            ),
            (
                "Organize link definitions and remove unused ones".to_string(),
                "Use [b][b] and [z].\n\nMore [g](guide.md) and [again](guide.md).\n\n[b]: guide.md\n[z]: guide.md\n".to_string(),
            ),
        ]
    );

    let extract = call::<CodeActionRequest>(
        &mut journey,
        11,
        CodeActionParams {
            text_document: document(&readme),
            range: range((7, 7), (7, 7)),
            context: CodeActionContext {
                diagnostics: Vec::new(),
                only: Some(vec![CodeActionKind::Refactor]),
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    )
    .await
    .unwrap();
    assert_eq!(extract.len(), 1);
    let (title, edits) = only_edit(&extract[0], &readme);
    assert_eq!(title, "Extract to link definition");
    let edits: Vec<_> = edits
        .into_iter()
        .map(|edit| (edit.range, edit.new_text))
        .collect();
    assert_eq!(
        edits,
        [
            (range((7, 8), (7, 18)), "[guide]".to_string()),
            (range((7, 30), (7, 40)), "[guide]".to_string()),
            (
                range((7, 41), (7, 41)),
                "\n\n[guide]: guide.md\n".to_string()
            ),
        ]
    );
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn watched_file_changes_republish_open_documents() {
    let fs = MemoryFs::new();
    let mut journey = start(&fs).await;
    let readme = uri("file:///w/readme.md");
    let published = open(&mut journey, &readme, "[g](guide.md)\n").await;
    assert_eq!(published.diagnostics.len(), 1);

    fs.insert(uri("file:///w/guide.md"), "# Guide\n");
    journey
        .peer()
        .send(notification(
            "workspace/didChangeWatchedFiles",
            &DidChangeWatchedFilesParams {
                changes: vec![FileEvent {
                    uri: uri("file:///w/guide.md"),
                    kind: FileChangeType::Created,
                }],
            },
        ))
        .unwrap();
    let republished = diagnostics(&mut journey).await;
    assert_eq!(republished.uri, readme);
    assert!(republished.diagnostics.is_empty());

    journey
        .peer()
        .send(notification(
            "workspace/didRenameFiles",
            &json!({ "files": [{ "oldUri": "file:///w/guide.md", "newUri": "file:///w/g2.md" }] }),
        ))
        .unwrap();
    assert!(diagnostics(&mut journey).await.diagnostics.is_empty());
    journey
        .peer()
        .send(notification(
            "textDocument/didClose",
            &json!({ "textDocument": { "uri": "file:///w/readme.md" } }),
        ))
        .unwrap();
    journey.finish().await.unwrap();
}

#[tokio::test]
async fn clients_with_dynamic_watchers_get_a_markdown_watcher_and_cached_files() {
    let fs = MemoryFs::new();
    fs.insert(uri("file:///w/guide.md"), "# Guide\n");
    let mut journey = start_with(
        &fs,
        json!({ "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } } }),
    )
    .await;
    let registration = support::next(&mut journey).await;
    let lspf::RawMessage::Request { id, method, params } = registration else {
        panic!("expected a registration request, got {registration:?}");
    };
    assert_eq!(method, "client/registerCapability");
    let params: Value = serde_json::from_slice(&params).unwrap();
    assert_eq!(
        params["registrations"][0]["registerOptions"]["watchers"][0]["globPattern"],
        "**/*.{md,markdown,mdown,mkd,mkdn}"
    );
    journey
        .peer()
        .send(lspf::RawMessage::Response {
            id,
            result: Ok(bytes::Bytes::from_static(b"null")),
        })
        .unwrap();

    let readme = uri("file:///w/readme.md");
    let published = open(&mut journey, &readme, "[g](guide.md#guide)\n").await;
    assert!(published.diagnostics.is_empty());

    // A reported change drops the guide from the index and republishes.
    fs.insert(uri("file:///w/guide.md"), "# Renamed\n");
    journey
        .peer()
        .send(notification(
            "workspace/didChangeWatchedFiles",
            &DidChangeWatchedFilesParams {
                changes: vec![FileEvent {
                    uri: uri("file:///w/guide.md"),
                    kind: FileChangeType::Changed,
                }],
            },
        ))
        .unwrap();
    let republished = diagnostics(&mut journey).await;
    assert_eq!(
        serde_json::to_value(&republished.diagnostics[0].message).unwrap(),
        json!("local link heading does not exist: guide.md#guide")
    );
    journey.finish().await.unwrap();
}
