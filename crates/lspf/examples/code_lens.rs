//! Code Lens server.
//! Lens commands are resolved lazily and apply the computed sum through the client.

mod example_support;

use std::collections::HashMap;
use std::sync::Arc;

use lspf::types::{
    ApplyWorkspaceEditParams, CodeLens, CodeLensOptions, CodeLensParams, Command, TextEdit,
    WorkspaceEdit,
};
use lspf::{CancellationToken, LspError, Server, ServerContext};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

struct State;

#[derive(Deserialize, Serialize)]
struct EvaluateSumArgs {
    uri: lspf::types::Uri,
    left: u64,
    right: u64,
    line: u32,
}

async fn code_lens(
    _: Arc<State>,
    ctx: ServerContext,
    params: CodeLensParams,
    _: CancellationToken,
) -> Result<Option<Vec<CodeLens>>, LspError> {
    let text = example_support::text(&ctx, &params.text_document.uri)?;
    Ok(Some(
        text.lines()
            .enumerate()
            .filter_map(|(line_number, line)| {
                let (left, right) = example_support::parse_incomplete_sum(line)?;
                Some(CodeLens {
                    range: example_support::line_range(line_number as u32, line),
                    command: None,
                    data: Some(json!({
                        "uri": params.text_document.uri,
                        "left": left,
                        "right": right,
                    })),
                })
            })
            .collect(),
    ))
}

async fn resolve(
    _: Arc<State>,
    _: ServerContext,
    mut lens: CodeLens,
    _: CancellationToken,
) -> Result<CodeLens, LspError> {
    let data = lens
        .data
        .take()
        .ok_or_else(|| LspError::invalid_params("code lens has no data"))?;
    let uri = data
        .get("uri")
        .cloned()
        .ok_or_else(|| LspError::invalid_params("missing uri"))?;
    let args = EvaluateSumArgs {
        uri: serde_json::from_value(uri).map_err(|_| LspError::invalid_params("invalid uri"))?,
        left: data
            .get("left")
            .and_then(Value::as_u64)
            .ok_or_else(|| LspError::invalid_params("missing left operand"))?,
        right: data
            .get("right")
            .and_then(Value::as_u64)
            .ok_or_else(|| LspError::invalid_params("missing right operand"))?,
        line: lens.range.start.line,
    };
    lens.command = Some(Command::new(
        format!("Evaluate {} + {}", args.left, args.right),
        None,
        "codeLens.evaluateSum".to_string(),
        Some(vec![json!(args)]),
    ));
    Ok(lens)
}

async fn evaluate_sum(
    _: Arc<State>,
    ctx: ServerContext,
    (args,): (EvaluateSumArgs,),
    _: CancellationToken,
) -> Result<(), LspError> {
    let text = example_support::text(&ctx, &args.uri)?;
    let line = text.lines().nth(args.line as usize).unwrap_or_default();
    let edit = WorkspaceEdit {
        changes: Some(HashMap::from([(
            args.uri,
            vec![TextEdit {
                range: example_support::line_range(args.line, line),
                new_text: format!("{} {}", line.trim(), args.left + args.right),
            }],
        )])),
        ..WorkspaceEdit::default()
    };
    ctx.client()
        .apply_edit(ApplyWorkspaceEditParams {
            label: None,
            edit,
            metadata: None,
        })
        .await
        .map_err(|error| LspError::internal(error.to_string()))?;
    Ok(())
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .feature(
            lspf::features::code_lens(CodeLensOptions {
                resolve_provider: None,
                ..Default::default()
            }),
            code_lens,
        )
        .feature(lspf::features::code_lens_resolve(), resolve)
        .command::<(EvaluateSumArgs,), (), _, _>("codeLens.evaluateSum", evaluate_sum)
        .build()
        .expect("code-lens registrations are valid");
    example_support::serve(server).await
}
