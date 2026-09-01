//! Code Actions server.
//! Incomplete sums such as `1 + 2 =` receive a quick-fix edit.

mod example_support;

use std::collections::HashMap;
use std::sync::Arc;

use lspf::types::{
    CodeAction, CodeActionKind, CodeActionOptions, CodeActionOrCommand, CodeActionParams,
    CodeActionResponse, TextEdit, WorkspaceEdit,
};
use lspf::{CancellationToken, LspError, Server, ServerContext};

struct State;

async fn code_actions(
    _: Arc<State>,
    ctx: ServerContext,
    params: CodeActionParams,
    _: CancellationToken,
) -> Result<Option<Vec<CodeActionResponse>>, LspError> {
    let uri = params.text_document.uri;
    let text = example_support::text(&ctx, &uri)?;
    let mut actions = Vec::new();
    for (line_number, line) in text
        .lines()
        .enumerate()
        .skip(params.range.start.line as usize)
    {
        if line_number > params.range.end.line as usize {
            break;
        }
        let Some((left, right)) = example_support::parse_incomplete_sum(line) else {
            continue;
        };
        let edit = TextEdit {
            range: example_support::line_range(line_number as u32, line),
            new_text: format!("{} {}!", line.trim(), left + right),
        };
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: format!("Evaluate '{}'", line.trim()),
            kind: Some(CodeActionKind::QuickFix),
            edit: Some(WorkspaceEdit {
                changes: Some(HashMap::from([(uri.clone(), vec![edit])])),
                ..WorkspaceEdit::default()
            }),
            ..CodeAction::default()
        }));
    }
    Ok(Some(actions))
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .feature(
            lspf::features::code_action(CodeActionOptions {
                code_action_kinds: Some(vec![CodeActionKind::QuickFix]),
                ..CodeActionOptions::default()
            }),
            code_actions,
        )
        .build()
        .expect("code-action registrations are valid");
    example_support::serve(server).await
}
