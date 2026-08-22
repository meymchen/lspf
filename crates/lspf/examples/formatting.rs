//! Markdown-table formatting server.
//! It implements whole-document, range, and on-type formatting.

mod example_support;

use std::sync::Arc;

use lspf::types::{
    DocumentFormattingOptions, DocumentFormattingParams, DocumentOnTypeFormattingOptions,
    DocumentOnTypeFormattingParams, DocumentRangeFormattingOptions, DocumentRangeFormattingParams,
    TextEdit,
};
use lspf::{CancellationToken, Context, LspError, Server};

struct State;

fn format_tables(text: &str, first_line: usize, last_line: usize) -> Vec<TextEdit> {
    text.lines()
        .enumerate()
        .filter(|(line, text)| {
            (*line >= first_line && *line <= last_line) && text.matches('|').count() >= 2
        })
        .map(|(line, text)| {
            let formatted = text
                .trim()
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" | ");
            TextEdit {
                range: example_support::line_range(line as u32, text),
                new_text: format!("| {formatted} |"),
            }
        })
        .collect()
}

async fn document_formatting(
    _: Arc<State>,
    ctx: Context,
    params: DocumentFormattingParams,
    _: CancellationToken,
) -> Result<Option<Vec<TextEdit>>, LspError> {
    let text = example_support::text(&ctx, &params.text_document.uri)?;
    Ok(Some(format_tables(&text, 0, usize::MAX)))
}

async fn range_formatting(
    _: Arc<State>,
    ctx: Context,
    params: DocumentRangeFormattingParams,
    _: CancellationToken,
) -> Result<Option<Vec<TextEdit>>, LspError> {
    let text = example_support::text(&ctx, &params.text_document.uri)?;
    Ok(Some(format_tables(
        &text,
        params.range.start.line as usize,
        params.range.end.line as usize,
    )))
}

async fn on_type_formatting(
    _: Arc<State>,
    ctx: Context,
    params: DocumentOnTypeFormattingParams,
    _: CancellationToken,
) -> Result<Option<Vec<TextEdit>>, LspError> {
    let uri = params.text_document_position.text_document.uri;
    let line = params.text_document_position.position.line as usize;
    let text = example_support::text(&ctx, &uri)?;
    Ok(Some(format_tables(&text, line, line)))
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .feature(
            lspf::features::document_formatting(DocumentFormattingOptions::default()),
            document_formatting,
        )
        .feature(
            lspf::features::range_formatting(DocumentRangeFormattingOptions {
                work_done_progress_options: Default::default(),
            }),
            range_formatting,
        )
        .feature(
            lspf::features::on_type_formatting(DocumentOnTypeFormattingOptions {
                first_trigger_character: "|".to_string(),
                more_trigger_character: None,
            }),
            on_type_formatting,
        )
        .build()
        .expect("formatting registrations are valid");
    example_support::serve(server).await
}
