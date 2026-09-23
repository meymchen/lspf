//! Shared helpers for the Markdown server's integration journeys.

#![allow(dead_code)]

use std::borrow::Cow;
use std::str::FromStr;
use std::time::Duration;

use bytes::Bytes;
use lspf::testing::ServerJourney;
use lspf::types::request::Request;
use lspf::types::{
    DidOpenTextDocumentParams, InitializeParams, Position, PublishDiagnosticsParams,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, Uri,
};
use lspf::{RawMessage, RequestId};
use lspf_markdown::MemoryFs;
use serde_json::{Value, json};

pub fn uri(value: &str) -> Uri {
    Uri::from_str(value).expect("the test URI parses")
}

pub fn notification(method: &'static str, params: &impl serde::Serialize) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(params).expect("notification params serialize")),
    }
}

pub fn request(id: i32, method: &'static str, params: &impl serde::Serialize) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(params).expect("request params serialize")),
    }
}

/// Initialize parameters for a client with one workspace root at
/// `file:///w/`, plus any extra client capabilities.
pub fn workspace_params(capabilities: Value) -> InitializeParams {
    serde_json::from_value(json!({
        "processId": null,
        "rootUri": "file:///w/",
        "workspaceFolders": [{ "uri": "file:///w/", "name": "w" }],
        "capabilities": capabilities,
    }))
    .expect("initialize params decode")
}

/// Start a server over `fs` with a `file:///w/` workspace root.
pub async fn start(fs: &MemoryFs) -> ServerJourney {
    start_with(fs, json!({})).await
}

pub async fn start_with(fs: &MemoryFs, capabilities: Value) -> ServerJourney {
    ServerJourney::start_with(
        lspf_markdown::server(fs.clone()),
        workspace_params(capabilities),
    )
    .await
    .expect("the journey initializes")
}

/// Receive the next message, failing after one second.
pub async fn next(journey: &mut ServerJourney) -> RawMessage {
    tokio::time::timeout(Duration::from_secs(1), journey.peer().recv())
        .await
        .expect("the server sends a message")
        .expect("the testing Transport stays open")
}

pub async fn diagnostics(journey: &mut ServerJourney) -> PublishDiagnosticsParams {
    let message = next(journey).await;
    let RawMessage::Notification { method, params } = message else {
        panic!("expected diagnostics notification, got {message:?}");
    };
    assert_eq!(method, "textDocument/publishDiagnostics");
    serde_json::from_slice(&params).expect("diagnostics params decode")
}

/// Open a Markdown document and return its first published diagnostics.
pub async fn open(journey: &mut ServerJourney, uri: &Uri, text: &str) -> PublishDiagnosticsParams {
    journey
        .peer()
        .send(notification(
            "textDocument/didOpen",
            &DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "markdown".into(),
                    version: 1,
                    text: text.into(),
                },
            },
        ))
        .unwrap();
    diagnostics(journey).await
}

/// Send one request and decode its successful result, skipping any
/// notifications that arrive first.
pub async fn call<R: Request>(journey: &mut ServerJourney, id: i32, params: R::Params) -> R::Result
where
    R::Params: serde::Serialize,
    R::Result: serde::de::DeserializeOwned,
{
    let method: &'static str = R::METHOD;
    journey.peer().send(request(id, method, &params)).unwrap();
    loop {
        match next(journey).await {
            RawMessage::Response {
                id: response_id,
                result,
            } if response_id == RequestId::Number(id) => {
                let bytes = result.unwrap_or_else(|error| panic!("{method} failed: {error:?}"));
                return serde_json::from_slice(&bytes).expect("the result decodes");
            }
            RawMessage::Notification { .. } => {}
            other => panic!("unexpected message {other:?}"),
        }
    }
}

/// Send one request and return its JSON result or error, as raw JSON.
pub async fn call_raw(
    journey: &mut ServerJourney,
    id: i32,
    method: &'static str,
    params: Value,
) -> Result<Value, String> {
    journey.peer().send(request(id, method, &params)).unwrap();
    loop {
        match next(journey).await {
            RawMessage::Response {
                id: response_id,
                result,
            } if response_id == RequestId::Number(id) => {
                return result
                    .map(|bytes| serde_json::from_slice(&bytes).expect("the result decodes"))
                    .map_err(|error| format!("{error:?}"));
            }
            RawMessage::Notification { .. } => {}
            other => panic!("unexpected message {other:?}"),
        }
    }
}

pub fn at(uri: &Uri, line: u32, character: u32) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: Position::new(line, character),
    }
}

pub fn document(uri: &Uri) -> TextDocumentIdentifier {
    TextDocumentIdentifier { uri: uri.clone() }
}
