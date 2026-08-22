//! Semantic Tokens server for a small `type`/`fn` language.

mod example_support;

use std::sync::Arc;

use lspf::types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensDeltaParams, SemanticTokensFullDeltaResult, SemanticTokensLegend,
    SemanticTokensOptions, SemanticTokensParams, SemanticTokensRangeParams,
    SemanticTokensRangeResult, SemanticTokensResult,
};
use lspf::{CancellationToken, Context, LspError, Server};

struct State;

fn classify(word: &str, line: &str, start: usize) -> u32 {
    if matches!(word, "type" | "fn") {
        return 0;
    }
    let prefix = &line[..start];
    if prefix.trim_end() == "type" {
        return 5;
    }
    if prefix.trim_end() == "fn" {
        return 2;
    }
    if let Some(open) = line.find('(')
        && let Some(close) = line.find(')')
        && start > open
        && start < close
        && line[start + word.len()..close]
            .trim_start()
            .starts_with(':')
    {
        return 4;
    }
    1
}

fn tokens(text: &str, start_line: u32, end_line: u32) -> SemanticTokens {
    let mut absolute = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        let line_number = line_number as u32;
        if line_number < start_line || line_number > end_line {
            continue;
        }
        let bytes = line.as_bytes();
        let mut start = 0;
        while start < bytes.len() {
            if bytes[start].is_ascii_alphabetic() || bytes[start] == b'_' {
                let mut end = start + 1;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
                absolute.push((
                    line_number,
                    start as u32,
                    (end - start) as u32,
                    classify(&line[start..end], line, start),
                ));
                start = end;
            } else if b"=+-*/".contains(&bytes[start]) {
                absolute.push((line_number, start as u32, 1, 3));
                start += 1;
            } else {
                start += 1;
            }
        }
    }
    let mut previous_line = 0;
    let mut previous_start = 0;
    let data = absolute
        .into_iter()
        .map(|(line, start, length, token_type)| {
            let delta_line = line - previous_line;
            let delta_start = if delta_line == 0 {
                start - previous_start
            } else {
                start
            };
            previous_line = line;
            previous_start = start;
            SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type,
                token_modifiers_bitset: 0,
            }
        })
        .collect();
    SemanticTokens {
        result_id: Some(format!("{start_line}:{end_line}")),
        data,
    }
}

async fn full(
    _: Arc<State>,
    ctx: Context,
    params: SemanticTokensParams,
    _: CancellationToken,
) -> Result<Option<SemanticTokensResult>, LspError> {
    let text = example_support::text(&ctx, &params.text_document.uri)?;
    Ok(Some(tokens(&text, 0, u32::MAX).into()))
}

async fn delta(
    _: Arc<State>,
    ctx: Context,
    params: SemanticTokensDeltaParams,
    _: CancellationToken,
) -> Result<Option<SemanticTokensFullDeltaResult>, LspError> {
    let text = example_support::text(&ctx, &params.text_document.uri)?;
    Ok(Some(tokens(&text, 0, u32::MAX).into()))
}

async fn range(
    _: Arc<State>,
    ctx: Context,
    params: SemanticTokensRangeParams,
    _: CancellationToken,
) -> Result<Option<SemanticTokensRangeResult>, LspError> {
    let text = example_support::text(&ctx, &params.text_document.uri)?;
    Ok(Some(
        tokens(&text, params.range.start.line, params.range.end.line).into(),
    ))
}

fn options() -> SemanticTokensOptions {
    SemanticTokensOptions {
        work_done_progress_options: Default::default(),
        legend: SemanticTokensLegend {
            token_types: vec![
                SemanticTokenType::KEYWORD,
                SemanticTokenType::VARIABLE,
                SemanticTokenType::FUNCTION,
                SemanticTokenType::OPERATOR,
                SemanticTokenType::PARAMETER,
                SemanticTokenType::TYPE,
            ],
            token_modifiers: vec![SemanticTokenModifier::DEFINITION],
        },
        range: None,
        full: None,
    }
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .feature(lspf::features::semantic_tokens_full(options()), full)
        .feature(lspf::features::semantic_tokens_full_delta(options()), delta)
        .feature(lspf::features::semantic_tokens_range(options()), range)
        .build()
        .expect("semantic-token registrations are valid");
    example_support::serve(server).await
}
