use std::time::Duration;

use bytes::Bytes;
use lspf::testing::ServerJourney;
use lspf::{Outcome, RawMessage, RequestId};
use serde_json::{Value, json};

use super::*;

const NOTEBOOK: &str = "file:///example.ipynb";
const FIRST: &str = "file:///example.ipynb#first";
const SECOND: &str = "file:///example.ipynb#second";
const THIRD: &str = "file:///example.ipynb#third";

fn notify(journey: &mut ServerJourney, method: &'static str, params: Value) {
    journey
        .peer()
        .send(RawMessage::Notification {
            method: method.into(),
            params: Bytes::from(serde_json::to_vec(&params).unwrap()),
        })
        .unwrap();
}

async fn recv(journey: &mut ServerJourney) -> RawMessage {
    tokio::time::timeout(Duration::from_secs(2), journey.peer().recv())
        .await
        .expect("the example responds without a wall-clock sleep")
        .expect("the example remains connected")
}

async fn expect_log(journey: &mut ServerJourney, expected: &str) {
    let message = recv(journey).await;
    let RawMessage::Notification { method, params } = message else {
        panic!("expected a lifecycle log, got {message:?}");
    };
    assert_eq!(method, "window/logMessage");
    let log: LogMessageParams = serde_json::from_slice(&params).unwrap();
    assert_eq!(log.kind, MessageType::Info);
    assert_eq!(log.message, expected);
}

async fn cell_hover(journey: &mut ServerJourney, uri: &str) -> Option<String> {
    journey
        .peer()
        .send(RawMessage::Request {
            id: RequestId::Number(10),
            method: "textDocument/hover".into(),
            params: Bytes::from(
                serde_json::to_vec(&json!({
                    "textDocument": {"uri": uri}, "position": {"line": 0, "character": 0}
                }))
                .unwrap(),
            ),
        })
        .unwrap();
    let message = recv(journey).await;
    let RawMessage::Response { id, result } = message else {
        panic!("expected hover, got {message:?}");
    };
    assert_eq!(id, RequestId::Number(10));
    let hover: Option<Hover> = serde_json::from_slice(&result.unwrap()).unwrap();
    hover.map(|hover| {
        let HoverContents::MarkupContent(contents) = hover.contents else {
            panic!("the example uses a plain-text hover");
        };
        assert_eq!(contents.kind, MarkupKind::PlainText);
        contents.value
    })
}

fn expected_hover(notebook_version: i32, cell_version: i32, text: &str) -> String {
    format!(
        "Notebook: {NOTEBOOK}\nNotebook version: {notebook_version}\nCells: 2\nCell version: {cell_version}\n\n{text}"
    )
}

async fn opened() -> ServerJourney {
    let mut journey = ServerJourney::start(server()).await.unwrap();
    notify(
        &mut journey,
        "notebookDocument/didOpen",
        json!({
            "notebookDocument": {
                "uri": NOTEBOOK, "notebookType": "jupyter-notebook", "version": 1,
                "cells": [{"kind": 2, "document": FIRST}, {"kind": 2, "document": SECOND}]
            },
            "cellTextDocuments": [
                {"uri": FIRST, "languageId": "plaintext", "version": 1, "text": "🙂 one"},
                {"uri": SECOND, "languageId": "plaintext", "version": 1, "text": "two"}
            ]
        }),
    );
    expect_log(
        &mut journey,
        &format!("open: {NOTEBOOK} (version 1, 2 cells)"),
    )
    .await;
    journey
}

#[tokio::test]
async fn advertises_notebook_sync_save_and_hover() {
    let journey = ServerJourney::start(server()).await.unwrap();
    let capture = journey.capture().snapshot();
    let result = capture
        .iter()
        .find_map(|event| match event.message() {
            RawMessage::Response {
                id: RequestId::Number(1),
                result: Ok(result),
            } => Some(serde_json::from_slice::<Value>(result).unwrap()),
            _ => None,
        })
        .expect("initialize was captured");
    assert_eq!(
        result["capabilities"]["notebookDocumentSync"],
        json!({
            "notebookSelector": [{"notebook": "jupyter-notebook"}], "save": true
        })
    );
    assert_eq!(result["capabilities"]["hoverProvider"], true);
    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });
}

#[tokio::test]
async fn lifecycle_hooks_and_hover_observe_synchronized_cell_text() {
    let mut journey = opened().await;
    assert_eq!(
        cell_hover(&mut journey, FIRST).await,
        Some(expected_hover(1, 1, "🙂 one"))
    );
    assert_eq!(
        cell_hover(&mut journey, SECOND).await,
        Some(expected_hover(1, 1, "two"))
    );
    notify(
        &mut journey,
        "notebookDocument/didChange",
        json!({
            "notebookDocument": {"uri": NOTEBOOK, "version": 2},
            "change": {"cells": {"textContent": [{
                "document": {"uri": FIRST, "version": 2},
                "changes": [{"range": {
                    "start": {"line": 0, "character": 3},
                    "end": {"line": 0, "character": 6}
                }, "text": "changed"}]
            }]}}
        }),
    );
    expect_log(
        &mut journey,
        &format!("change: {NOTEBOOK} (version 2, 2 cells)"),
    )
    .await;
    assert_eq!(
        cell_hover(&mut journey, FIRST).await,
        Some(expected_hover(2, 2, "🙂 changed"))
    );
    assert_eq!(
        cell_hover(&mut journey, SECOND).await,
        Some(expected_hover(2, 1, "two"))
    );
    notify(
        &mut journey,
        "notebookDocument/didSave",
        json!({"notebookDocument": {"uri": NOTEBOOK}}),
    );
    expect_log(
        &mut journey,
        &format!("save: {NOTEBOOK} (version 2, 2 cells)"),
    )
    .await;
    notify(
        &mut journey,
        "notebookDocument/didClose",
        json!({
            "notebookDocument": {"uri": NOTEBOOK}, "cellTextDocuments": []
        }),
    );
    expect_log(&mut journey, &format!("close: {NOTEBOOK} is closed")).await;
    assert_eq!(cell_hover(&mut journey, FIRST).await, None);
    assert_eq!(cell_hover(&mut journey, SECOND).await, None);
    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });
}

#[tokio::test]
async fn structural_changes_replace_cell_membership_and_text() {
    let mut journey = opened().await;
    notify(
        &mut journey,
        "notebookDocument/didChange",
        json!({
            "notebookDocument": {"uri": NOTEBOOK, "version": 2},
            "change": {"cells": {"structure": {
                "array": {"start": 0, "deleteCount": 1, "cells": [{"kind": 2, "document": THIRD}]},
                "didOpen": [{"uri": THIRD, "languageId": "plaintext", "version": 1, "text": "three"}]
            }}}
        }),
    );
    expect_log(
        &mut journey,
        &format!("change: {NOTEBOOK} (version 2, 2 cells)"),
    )
    .await;
    assert_eq!(cell_hover(&mut journey, FIRST).await, None);
    assert_eq!(
        cell_hover(&mut journey, THIRD).await,
        Some(expected_hover(2, 1, "three"))
    );
    assert_eq!(
        cell_hover(&mut journey, SECOND).await,
        Some(expected_hover(2, 1, "two"))
    );
    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });
}

#[tokio::test]
async fn a_rejected_batch_restores_repeated_edits_and_skips_the_change_hook() {
    let mut journey = opened().await;
    notify(
        &mut journey,
        "notebookDocument/didChange",
        json!({
            "notebookDocument": {"uri": NOTEBOOK, "version": 2},
            "change": {"cells": {"textContent": [
                {"document": {"uri": FIRST, "version": 2}, "changes": [{"text": "first edit"}]},
                {"document": {"uri": FIRST, "version": 3}, "changes": [{"text": "second edit"}]},
                {"document": {"uri": THIRD, "version": 1}, "changes": [{"text": "not open"}]}
            ]}}
        }),
    );
    // A wrongly dispatched change hook would send a log before this response.
    assert_eq!(
        cell_hover(&mut journey, FIRST).await,
        Some(expected_hover(1, 1, "🙂 one"))
    );
    assert_eq!(
        cell_hover(&mut journey, SECOND).await,
        Some(expected_hover(1, 1, "two"))
    );
    assert_eq!(cell_hover(&mut journey, THIRD).await, None);
    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });
}
