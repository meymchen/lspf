//! Wire-level coverage for inbound methods added in LSP 3.18.

use std::borrow::Cow;
use std::sync::Arc;

use bytes::Bytes;
use lspf::testing::{ServerJourney, WireCapture};
use lspf::types::request::InlineCompletionRequest;
use lspf::types::{
    DocumentRangeFormattingOptions, DocumentRangeFormattingParams, DocumentRangesFormattingParams,
    InlineCompletionItem, InlineCompletionOptions, InlineCompletionParams,
    InlineCompletionResponse, InsertText, Position, Range, TextDocumentContentOptions,
    TextDocumentContentParams, TextDocumentContentResult, TextEdit, WorkDoneProgressOptions,
};
use lspf::{
    BuildError, CancellationToken, LspError, Outcome, RawMessage, RequestId, Server, ServerContext,
};
use serde_json::{Value, json};

async fn inline_completion(
    _: Arc<()>,
    _: ServerContext,
    _: InlineCompletionParams,
    _: CancellationToken,
) -> Result<Option<InlineCompletionResponse>, LspError> {
    Ok(Some(
        vec![InlineCompletionItem::new(
            InsertText::String("completion".to_string()),
            None,
            None,
            None,
        )]
        .into(),
    ))
}

async fn text_document_content(
    _: Arc<()>,
    _: ServerContext,
    params: TextDocumentContentParams,
    _: CancellationToken,
) -> Result<TextDocumentContentResult, LspError> {
    Ok(TextDocumentContentResult::new(format!(
        "content for {}",
        params.uri
    )))
}

async fn range_formatting(
    _: Arc<()>,
    _: ServerContext,
    _: DocumentRangeFormattingParams,
    _: CancellationToken,
) -> Result<Option<Vec<TextEdit>>, LspError> {
    Ok(None)
}

async fn ranges_formatting(
    _: Arc<()>,
    _: ServerContext,
    params: DocumentRangesFormattingParams,
    _: CancellationToken,
) -> Result<Option<Vec<TextEdit>>, LspError> {
    Ok(Some(vec![TextEdit {
        range: params.ranges[0],
        new_text: "formatted ranges".to_string(),
    }]))
}

fn request(id: i32, method: &'static str, params: Value) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).expect("request params serialize")),
    }
}

fn successful_response(capture: &WireCapture, id: i32) -> Value {
    capture
        .snapshot()
        .into_iter()
        .find_map(|event| match event.message() {
            RawMessage::Response {
                id: RequestId::Number(response_id),
                result: Ok(body),
            } if *response_id == id => {
                Some(serde_json::from_slice(body).expect("response body is JSON"))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("successful response {id}"))
}

#[tokio::test]
async fn inline_completion_advertises_and_serves_at_the_transport_seam() {
    let server = Server::builder(())
        .feature(
            lspf::features::inline_completion(InlineCompletionOptions::default()),
            inline_completion,
        )
        .build()
        .expect("inline completion builds");
    let mut journey = ServerJourney::start(server).await.expect("server starts");
    let capture = journey.capture();

    assert_eq!(
        successful_response(&capture, 1)["capabilities"]["inlineCompletionProvider"],
        json!({})
    );

    journey
        .peer()
        .send(request(
            10,
            "textDocument/inlineCompletion",
            json!({
                "textDocument": { "uri": "file:///main.rs" },
                "position": { "line": 0, "character": 3 },
                "context": { "triggerKind": 1 }
            }),
        ))
        .expect("request reaches server");
    journey.peer().recv().await.expect("server responds");

    assert_eq!(
        successful_response(&capture, 10)[0]["insertText"],
        "completion"
    );
    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });
}

#[tokio::test]
async fn workspace_text_document_content_advertises_and_serves_at_the_transport_seam() {
    let server = Server::builder(())
        .feature(
            lspf::features::text_document_content(TextDocumentContentOptions::new(vec![
                "git".to_string(),
            ])),
            text_document_content,
        )
        .build()
        .expect("workspace text document content builds");
    let mut journey = ServerJourney::start(server).await.expect("server starts");
    let capture = journey.capture();

    assert_eq!(
        successful_response(&capture, 1)["capabilities"]["workspace"]["textDocumentContent"],
        json!({ "schemes": ["git"] })
    );

    journey
        .peer()
        .send(request(
            10,
            "workspace/textDocumentContent",
            json!({ "uri": "git:/revision/main.rs" }),
        ))
        .expect("request reaches server");
    journey.peer().recv().await.expect("server responds");

    assert_eq!(
        successful_response(&capture, 10)["text"],
        "content for git:/revision/main.rs"
    );
    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });
}

#[tokio::test]
async fn ranges_formatting_merges_with_range_formatting_and_serves_at_the_transport_seam() {
    let server = Server::builder(())
        .feature(
            lspf::features::range_formatting(DocumentRangeFormattingOptions {
                ranges_support: None,
                work_done_progress_options: WorkDoneProgressOptions {
                    work_done_progress: Some(true),
                },
            }),
            range_formatting,
        )
        .feature(
            lspf::features::ranges_formatting(DocumentRangeFormattingOptions {
                ranges_support: None,
                work_done_progress_options: WorkDoneProgressOptions {
                    work_done_progress: Some(true),
                },
            }),
            ranges_formatting,
        )
        .build()
        .expect("range and ranges formatting build as one family");
    let mut journey = ServerJourney::start(server).await.expect("server starts");
    let capture = journey.capture();

    assert_eq!(
        successful_response(&capture, 1)["capabilities"]["documentRangeFormattingProvider"],
        json!({ "rangesSupport": true, "workDoneProgress": true })
    );

    journey
        .peer()
        .send(request(
            10,
            "textDocument/rangesFormatting",
            json!({
                "textDocument": { "uri": "file:///main.rs" },
                "ranges": [{
                    "start": { "line": 1, "character": 0 },
                    "end": { "line": 1, "character": 4 }
                }],
                "options": { "tabSize": 4, "insertSpaces": true }
            }),
        ))
        .expect("request reaches server");
    journey.peer().recv().await.expect("server responds");

    assert_eq!(
        successful_response(&capture, 10),
        json!([{
            "range": Range::new(Position::new(1, 0), Position::new(1, 4)),
            "newText": "formatted ranges"
        }])
    );
    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });
}

#[test]
fn duplicate_lsp_318_features_are_conflicting_capabilities() {
    let inline = Server::builder(())
        .feature(
            lspf::features::inline_completion(InlineCompletionOptions::default()),
            inline_completion,
        )
        .feature(
            lspf::features::inline_completion(InlineCompletionOptions::default()),
            inline_completion,
        )
        .build()
        .err()
        .expect("duplicate inline completion fails");
    assert_eq!(
        inline,
        BuildError::ConflictingCapability {
            field: "inlineCompletionProvider"
        }
    );

    let content_options = || TextDocumentContentOptions::new(vec!["git".to_string()]);
    let content = Server::builder(())
        .feature(
            lspf::features::text_document_content(content_options()),
            text_document_content,
        )
        .feature(
            lspf::features::text_document_content(content_options()),
            text_document_content,
        )
        .build()
        .err()
        .expect("duplicate text document content fails");
    assert_eq!(
        content,
        BuildError::ConflictingCapability {
            field: "workspace.textDocumentContent"
        }
    );

    let range_options = || DocumentRangeFormattingOptions::default();
    let ranges = Server::builder(())
        .feature(
            lspf::features::ranges_formatting(range_options()),
            ranges_formatting,
        )
        .feature(
            lspf::features::ranges_formatting(range_options()),
            ranges_formatting,
        )
        .build()
        .err()
        .expect("duplicate ranges formatting fails");
    assert_eq!(
        ranges,
        BuildError::ConflictingCapability {
            field: "documentRangeFormattingProvider"
        }
    );
}

#[test]
fn custom_and_feature_handlers_for_the_same_lsp_318_method_are_duplicate_methods() {
    let custom_first = Server::builder(())
        .request::<InlineCompletionRequest, _, _>(inline_completion)
        .feature(
            lspf::features::inline_completion(InlineCompletionOptions::default()),
            inline_completion,
        )
        .build()
        .err()
        .expect("feature cannot replace a custom handler");
    assert_eq!(
        custom_first,
        BuildError::DuplicateMethod("textDocument/inlineCompletion".to_string())
    );

    let feature_first = Server::builder(())
        .feature(
            lspf::features::inline_completion(InlineCompletionOptions::default()),
            inline_completion,
        )
        .request::<InlineCompletionRequest, _, _>(inline_completion)
        .build()
        .err()
        .expect("custom handler cannot replace a feature");
    assert_eq!(
        feature_first,
        BuildError::DuplicateMethod("textDocument/inlineCompletion".to_string())
    );
}

#[tokio::test]
async fn lsp_318_capability_fields_are_absent_without_their_registrations() {
    let journey = ServerJourney::start(Server::builder(()).build().expect("empty server builds"))
        .await
        .expect("server starts");
    let capture = journey.capture();
    let capabilities = &successful_response(&capture, 1)["capabilities"];

    assert!(capabilities.get("inlineCompletionProvider").is_none());
    assert!(
        capabilities
            .get("documentRangeFormattingProvider")
            .is_none()
    );
    assert!(
        capabilities["workspace"]
            .get("textDocumentContent")
            .is_none()
    );
    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });
}

#[tokio::test]
async fn ranges_support_is_contributed_only_by_ranges_formatting() {
    let range_only = Server::builder(())
        .feature(
            lspf::features::range_formatting(DocumentRangeFormattingOptions::default()),
            range_formatting,
        )
        .build()
        .expect("range formatting builds");
    let range_journey = ServerJourney::start(range_only)
        .await
        .expect("server starts");
    let range_capture = range_journey.capture();
    assert_eq!(
        successful_response(&range_capture, 1)["capabilities"]["documentRangeFormattingProvider"],
        json!({})
    );
    assert_eq!(
        range_journey.finish().await.unwrap(),
        Outcome::Exit { code: 0 }
    );

    let ranges_only = Server::builder(())
        .feature(
            lspf::features::ranges_formatting(DocumentRangeFormattingOptions::default()),
            ranges_formatting,
        )
        .build()
        .expect("ranges formatting builds");
    let ranges_journey = ServerJourney::start(ranges_only)
        .await
        .expect("server starts");
    let ranges_capture = ranges_journey.capture();
    assert_eq!(
        successful_response(&ranges_capture, 1)["capabilities"]["documentRangeFormattingProvider"],
        json!({ "rangesSupport": true })
    );
    assert_eq!(
        ranges_journey.finish().await.unwrap(),
        Outcome::Exit { code: 0 }
    );
}
