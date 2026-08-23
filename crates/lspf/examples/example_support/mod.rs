//! Shared utilities for the feature example servers.

#![allow(dead_code)]

use lspf::types::{Diagnostic, DiagnosticSeverity, Position, Range, Uri};
use lspf::{Context, LspError, Server};

pub(crate) async fn serve<S: Send + Sync + 'static>(server: Server<S>) -> lspf::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        // These stdio servers are normally launched by an editor, whose output
        // channel does not interpret terminal formatting.
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let outcome = lspf::stdio(server).serve().await?;
    std::process::exit(outcome.code());
}

pub(crate) fn text(ctx: &Context, uri: &Uri) -> Result<String, LspError> {
    ctx.documents()
        .get(uri)
        .map(|document| document.text())
        .ok_or_else(|| LspError::invalid_params(format!("document is not open: {}", uri.as_str())))
}

pub(crate) fn line_range(line: u32, text: &str) -> Range {
    Range::new(
        Position::new(line, 0),
        Position::new(line, text.trim_end_matches(['\r', '\n']).len() as u32),
    )
}

pub(crate) fn parse_incomplete_sum(line: &str) -> Option<(u64, u64)> {
    let expression = line.trim().strip_suffix('=')?.trim();
    let (left, right) = expression.split_once('+')?;
    Some((left.trim().parse().ok()?, right.trim().parse().ok()?))
}

pub(crate) fn parse_sum(line: &str) -> Option<(u64, u64, Option<u64>)> {
    let (expression, answer) = line.trim().split_once('=')?;
    let (left, right) = expression.split_once('+')?;
    let answer = match answer.trim() {
        "" => None,
        value => Some(value.parse().ok()?),
    };
    Some((
        left.trim().parse().ok()?,
        right.trim().parse().ok()?,
        answer,
    ))
}

pub(crate) fn sum_diagnostics(text: &str) -> Vec<Diagnostic> {
    text.lines()
        .enumerate()
        .filter_map(|(line_number, line)| {
            let (left, right, answer) = parse_sum(line)?;
            if answer == Some(left + right) {
                return None;
            }
            let (severity, message) = match answer {
                None => (DiagnosticSeverity::WARNING, "Missing answer".to_string()),
                Some(actual) => (
                    DiagnosticSeverity::ERROR,
                    format!("Incorrect answer: {actual}"),
                ),
            };
            Some(Diagnostic {
                range: line_range(line_number as u32, line),
                severity: Some(severity),
                source: Some("lspf-example".to_string()),
                message,
                ..Diagnostic::default()
            })
        })
        .collect()
}

pub(crate) fn word_at(text: &str, position: Position) -> Option<(String, Range)> {
    let line = text.lines().nth(position.line as usize)?;
    let cursor = usize::try_from(position.character).ok()?.min(line.len());
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut start = cursor;
    while start > 0 && is_word(line.as_bytes()[start - 1]) {
        start -= 1;
    }
    let mut end = cursor;
    while end < line.len() && is_word(line.as_bytes()[end]) {
        end += 1;
    }
    (start != end).then(|| {
        (
            line[start..end].to_string(),
            Range::new(
                Position::new(position.line, start as u32),
                Position::new(position.line, end as u32),
            ),
        )
    })
}

pub(crate) fn word_ranges(text: &str, needle: &str) -> Vec<Range> {
    text.lines()
        .enumerate()
        .flat_map(|(line, text)| {
            text.match_indices(needle).filter_map(move |(start, _)| {
                let end = start + needle.len();
                let boundary = |index: usize| {
                    text.as_bytes()
                        .get(index)
                        .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
                };
                (boundary(start.wrapping_sub(1)) && boundary(end)).then(|| {
                    Range::new(
                        Position::new(line as u32, start as u32),
                        Position::new(line as u32, end as u32),
                    )
                })
            })
        })
        .collect()
}
