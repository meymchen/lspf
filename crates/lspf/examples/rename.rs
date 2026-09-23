//! Rename server for a small `type`/`fn` language.

mod example_support;

use std::collections::HashMap;
use std::sync::Arc;

use lspf::types::{
    PrepareRenameParams, PrepareRenamePlaceholder, PrepareRenameResponse, RenameOptions,
    RenameParams, TextEdit, WorkspaceEdit,
};
use lspf::{CancellationToken, LspError, Server, ServerContext};

struct State;

fn renameable(text: &str, word: &str) -> bool {
    text.lines().any(|line| {
        ["type ", "fn "].into_iter().any(|prefix| {
            line.strip_prefix(prefix)
                .and_then(|rest| rest.split(['(', ' ', '=']).next())
                == Some(word)
        })
    })
}

async fn prepare(
    _: Arc<State>,
    ctx: ServerContext,
    params: PrepareRenameParams,
    _: CancellationToken,
) -> Result<Option<PrepareRenameResponse>, LspError> {
    let position = params.text_document_position_params;
    let document = example_support::document(&ctx, &position.text_document.uri)?;
    let Some((word, range)) = document.word_at_position(position.position, |ch| {
        ch.is_ascii_alphanumeric() || ch == '_'
    }) else {
        return Ok(None);
    };
    Ok(renameable(
        &document
            .text(None)
            .expect("full document text is always available"),
        &word,
    )
    .then_some(PrepareRenameResponse::PrepareRenamePlaceholder(
        PrepareRenamePlaceholder {
            range,
            placeholder: word.into_owned(),
        },
    )))
}

async fn rename(
    _: Arc<State>,
    ctx: ServerContext,
    params: RenameParams,
    _: CancellationToken,
) -> Result<Option<WorkspaceEdit>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let document = example_support::document(&ctx, &uri)?;
    let Some((word, _)) = document
        .word_at_position(params.text_document_position_params.position, |ch| {
            ch.is_ascii_alphanumeric() || ch == '_'
        })
    else {
        return Ok(None);
    };
    let text = document
        .text(None)
        .expect("full document text is always available");
    if !renameable(&text, &word) {
        return Ok(None);
    }
    let edits = example_support::word_ranges(&text, &word, document.position_encoding())
        .into_iter()
        .map(|range| TextEdit {
            range,
            new_text: params.new_name.clone(),
        })
        .collect();
    Ok(Some(WorkspaceEdit {
        changes: Some(HashMap::from([(uri, edits)])),
        ..WorkspaceEdit::default()
    }))
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .feature(
            lspf::features::rename(RenameOptions {
                prepare_provider: None,
                work_done_progress_options: Default::default(),
            }),
            rename,
        )
        .feature(lspf::features::prepare_rename(), prepare)
        .build()
        .expect("rename registrations are valid");
    example_support::serve(server).await
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use super::*;
    use example_support::text_tests::{URI, opened, request};
    use serde_json::json;

    #[tokio::test]
    async fn rename_selects_and_edits_words_after_a_utf16_emoji_prefix() {
        let server = Server::builder(State)
            .feature(lspf::features::rename(RenameOptions::default()), rename)
            .feature(lspf::features::prepare_rename(), prepare)
            .build()
            .unwrap();
        let mut journey = opened(server, "type Thing(\n\u{1f600} Thing").await;
        let params = json!({"textDocument":{"uri":URI},"position":{"line":1,"character":5}});
        let response = request(&mut journey, "textDocument/prepareRename", params.clone()).await;
        let selection = json!({"start":{"line":1,"character":3},"end":{"line":1,"character":8}});
        assert_eq!(response, json!({"range":selection,"placeholder":"Thing"}));
        let mut params = params;
        params["newName"] = json!("Renamed");
        let response = request(&mut journey, "textDocument/rename", params).await;
        assert_eq!(
            response["changes"][URI],
            json!([
                {"range":{"start":{"line":0,"character":5},"end":{"line":0,"character":10}},"newText":"Renamed"},
                {"range":selection,"newText":"Renamed"},
            ])
        );
        journey.finish().await.unwrap();
    }
}
