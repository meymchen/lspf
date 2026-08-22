//! Inlay Hints server.
//! Decimal integers receive binary labels; tooltips are deferred to resolve.

mod example_support;

use std::sync::Arc;

use lspf::types::{
    InlayHint, InlayHintKind, InlayHintLabel, InlayHintOptions, InlayHintParams, InlayHintTooltip,
    Position,
};
use lspf::{CancellationToken, Context, LspError, Server};

struct State;

async fn inlay_hints(
    _: Arc<State>,
    ctx: Context,
    params: InlayHintParams,
    _: CancellationToken,
) -> Result<Option<Vec<InlayHint>>, LspError> {
    let text = example_support::text(&ctx, &params.text_document.uri)?;
    let mut hints = Vec::new();
    for (line_number, line) in text
        .lines()
        .enumerate()
        .skip(params.range.start.line as usize)
    {
        if line_number > params.range.end.line as usize {
            break;
        }
        let bytes = line.as_bytes();
        let mut start = 0;
        while start < bytes.len() {
            if !bytes[start].is_ascii_digit() {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if let Ok(number) = line[start..end].parse::<u64>() {
                hints.push(InlayHint {
                    position: Position::new(line_number as u32, end as u32),
                    label: InlayHintLabel::String(format!(":{number:b}")),
                    kind: Some(InlayHintKind::TYPE),
                    text_edits: None,
                    tooltip: None,
                    padding_left: Some(false),
                    padding_right: Some(true),
                    data: None,
                });
            }
            start = end;
        }
    }
    Ok(Some(hints))
}

async fn resolve(
    _: Arc<State>,
    _: Context,
    mut hint: InlayHint,
    _: CancellationToken,
) -> Result<InlayHint, LspError> {
    if let InlayHintLabel::String(label) = &hint.label
        && let Ok(number) = u64::from_str_radix(label.trim_start_matches(':'), 2)
    {
        hint.tooltip = Some(InlayHintTooltip::String(format!(
            "Binary representation of the number: {number}"
        )));
    }
    Ok(hint)
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .feature(
            lspf::features::inlay_hint(InlayHintOptions::default()),
            inlay_hints,
        )
        .feature(lspf::features::inlay_hint_resolve(), resolve)
        .build()
        .expect("inlay-hint registrations are valid");
    example_support::serve(server).await
}
