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
    let text = example_support::text(&ctx, &position.text_document.uri)?;
    let Some((word, range)) = example_support::word_at(&text, position.position) else {
        return Ok(None);
    };
    Ok(
        renameable(&text, &word).then_some(PrepareRenameResponse::PrepareRenamePlaceholder(
            PrepareRenamePlaceholder {
                range,
                placeholder: word,
            },
        )),
    )
}

async fn rename(
    _: Arc<State>,
    ctx: ServerContext,
    params: RenameParams,
    _: CancellationToken,
) -> Result<Option<WorkspaceEdit>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let text = example_support::text(&ctx, &uri)?;
    let Some((word, _)) =
        example_support::word_at(&text, params.text_document_position_params.position)
    else {
        return Ok(None);
    };
    if !renameable(&text, &word) {
        return Ok(None);
    }
    let edits = example_support::word_ranges(&text, &word)
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
