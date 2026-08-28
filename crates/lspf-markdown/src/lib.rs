//! First-party Markdown link language server.

use std::str::FromStr;
use std::sync::Arc;

use lspf::types::notification::{DidChangeTextDocument, DidOpenTextDocument, PublishDiagnostics};
use lspf::types::{
    DefinitionOptions, Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidOpenTextDocumentParams, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents,
    HoverParams, Location, MarkupContent, MarkupKind, PublishDiagnosticsParams, Range, Uri,
};
use lspf::{CancellationToken, FileProvider, LspError, Server, ServerContext};

const SOURCE: &str = "lspf-markdown";

/// Application state for the Markdown server.
pub struct State;

#[derive(Debug)]
struct Link {
    target: String,
    target_start: usize,
    target_end: usize,
}

fn inside_inline_code(line: &str, offset: usize) -> bool {
    let bytes = line.as_bytes();
    let mut cursor = 0;
    let mut delimiter = None;
    while cursor < offset {
        if bytes[cursor] != b'`' {
            cursor += 1;
            continue;
        }
        let run_start = cursor;
        while cursor < offset && bytes[cursor] == b'`' {
            cursor += 1;
        }
        let run = cursor - run_start;
        match delimiter {
            Some(open) if open == run => delimiter = None,
            None => delimiter = Some(run),
            Some(_) => {}
        }
    }
    delimiter.is_some()
}

fn inline_links(text: &str) -> Vec<Link> {
    let mut links = Vec::new();
    let mut line_start = 0;
    let mut fenced = false;

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            line_start += line.len();
            continue;
        }
        if fenced {
            line_start += line.len();
            continue;
        }

        let mut cursor = 0;
        while let Some(relative_close) = line[cursor..].find("](") {
            let close = cursor + relative_close;
            if inside_inline_code(line, close) {
                cursor = close + 2;
                continue;
            }
            let Some(_open) = line[..close].rfind('[') else {
                cursor = close + 2;
                continue;
            };
            let destination_start = close + 2;
            let Some(relative_end) = line[destination_start..].find(')') else {
                break;
            };
            let destination_end = destination_start + relative_end;
            let raw = &line[destination_start..destination_end];
            let (target, leading, trailing) = if raw.starts_with('<') && raw.ends_with('>') {
                (&raw[1..raw.len() - 1], 1, 1)
            } else {
                let target_end = raw.find(char::is_whitespace).unwrap_or(raw.len());
                (&raw[..target_end], 0, raw.len() - target_end)
            };
            if !target.is_empty() {
                links.push(Link {
                    target: target.to_string(),
                    target_start: line_start + destination_start + leading,
                    target_end: line_start + destination_end - trailing,
                });
            }
            cursor = destination_end + 1;
            if cursor >= line.len() {
                break;
            }
        }
        line_start += line.len();
    }

    links
}

fn has_uri_scheme(target: &str) -> bool {
    let Some(colon) = target.find(':') else {
        return false;
    };
    !target[..colon].is_empty()
        && target[..colon].bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || (index > 0 && (byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')))
        })
}

fn normalize_uri_path(uri: &str) -> String {
    let Some(colon) = uri.find(':') else {
        return uri.to_string();
    };
    let after_scheme = colon + 1;
    let path_start = if uri[after_scheme..].starts_with("//") {
        uri[after_scheme + 2..]
            .find('/')
            .map_or(uri.len(), |slash| after_scheme + 2 + slash)
    } else {
        after_scheme
    };
    let (prefix, path) = uri.split_at(path_start);
    let absolute = path.starts_with('/');
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            value => segments.push(value),
        }
    }
    format!(
        "{prefix}{}{}",
        if absolute { "/" } else { "" },
        segments.join("/")
    )
}

fn resolve_local_target(source: &Uri, target: &str) -> Option<Uri> {
    let target = target.split(['#', '?']).next().unwrap_or_default();
    if target.is_empty() {
        return Some(source.clone());
    }
    if target.starts_with("//") {
        return None;
    }
    if has_uri_scheme(target) {
        return target
            .starts_with("file:")
            .then(|| Uri::from_str(target).ok())
            .flatten();
    }

    let source = source.as_str().split(['#', '?']).next().unwrap_or_default();
    let combined = if target.starts_with('/') {
        let colon = source.find(':')?;
        let after_scheme = colon + 1;
        if source[after_scheme..].starts_with("//") {
            let authority_end = source[after_scheme + 2..]
                .find('/')
                .map_or(source.len(), |slash| after_scheme + 2 + slash);
            format!("{}{}", &source[..authority_end], target)
        } else {
            format!("{}{}", &source[..after_scheme], target)
        }
    } else {
        let directory_end = source.rfind('/')? + 1;
        format!("{}{target}", &source[..directory_end])
    };
    Uri::from_str(&normalize_uri_path(&combined)).ok()
}

async fn publish_diagnostics(ctx: ServerContext, uri: Uri) {
    let Some(document) = ctx.documents().get(&uri) else {
        return;
    };
    let text = document.text();
    let mut diagnostics = Vec::new();
    for link in inline_links(&text) {
        let Some(target_uri) = resolve_local_target(&uri, &link.target) else {
            continue;
        };
        if ctx.workspace().text_document(&target_uri).await.is_ok() {
            continue;
        }
        let Some(start) = ctx.documents().offset_to_position(&uri, link.target_start) else {
            continue;
        };
        let Some(end) = ctx.documents().offset_to_position(&uri, link.target_end) else {
            continue;
        };
        diagnostics.push(Diagnostic {
            range: Range::new(start, end),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some(SOURCE.into()),
            message: format!("local link target does not exist: {}", link.target),
            ..Diagnostic::default()
        });
    }

    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: document.version(),
    };
    let _ = ctx.client().notify::<PublishDiagnostics>(params);
}

async fn did_open(_state: Arc<State>, ctx: ServerContext, params: DidOpenTextDocumentParams) {
    publish_diagnostics(ctx, params.text_document.uri).await;
}

async fn did_change(_state: Arc<State>, ctx: ServerContext, params: DidChangeTextDocumentParams) {
    publish_diagnostics(ctx, params.text_document.uri).await;
}

async fn hover(
    _state: Arc<State>,
    ctx: ServerContext,
    params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let Some(document) = ctx.documents().get(&uri) else {
        return Ok(None);
    };
    let Some(offset) = document.position_to_offset(
        ctx.documents().position_encoding(),
        params.text_document_position_params.position,
    ) else {
        return Ok(None);
    };
    let Some(link) = inline_links(&document.text())
        .into_iter()
        .find(|link| link.target_start <= offset && offset <= link.target_end)
    else {
        return Ok(None);
    };
    let Some(target_uri) = resolve_local_target(&uri, &link.target) else {
        return Ok(None);
    };
    let Ok(target) = ctx.workspace().text_document(&target_uri).await else {
        return Ok(None);
    };
    let heading = target
        .text()
        .lines()
        .find_map(|line| {
            let trimmed = line.trim_start();
            let title = trimmed.trim_start_matches('#').trim_start();
            (trimmed.starts_with('#') && !title.is_empty()).then(|| title.to_string())
        })
        .unwrap_or_else(|| link.target.clone());
    let Some(start) =
        document.offset_to_position(ctx.documents().position_encoding(), link.target_start)
    else {
        return Ok(None);
    };
    let Some(end) =
        document.offset_to_position(ctx.documents().position_encoding(), link.target_end)
    else {
        return Ok(None);
    };

    Ok(Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("**{heading}**\n\n`{}`", target_uri.as_str()),
        }),
        range: Some(Range::new(start, end)),
    }))
}

async fn definition(
    _state: Arc<State>,
    ctx: ServerContext,
    params: GotoDefinitionParams,
    _ct: CancellationToken,
) -> Result<Option<GotoDefinitionResponse>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let Some(document) = ctx.documents().get(&uri) else {
        return Ok(None);
    };
    let Some(offset) = document.position_to_offset(
        ctx.documents().position_encoding(),
        params.text_document_position_params.position,
    ) else {
        return Ok(None);
    };
    let Some(link) = inline_links(&document.text())
        .into_iter()
        .find(|link| link.target_start <= offset && offset <= link.target_end)
    else {
        return Ok(None);
    };
    let Some(target_uri) = resolve_local_target(&uri, &link.target) else {
        return Ok(None);
    };
    let Ok(target) = ctx.workspace().text_document(&target_uri).await else {
        return Ok(None);
    };
    let target_text = target.text();
    let mut line_start = 0;
    let mut heading_offsets = None;
    for line in target_text.split_inclusive('\n') {
        let leading = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
            let after_hashes = &trimmed[hashes..];
            let spacing = after_hashes.len() - after_hashes.trim_start().len();
            let title = after_hashes.trim().trim_end_matches('#').trim_end();
            if !title.is_empty() {
                let start = line_start + leading + hashes + spacing;
                heading_offsets = Some((start, start + title.len()));
                break;
            }
        }
        line_start += line.len();
    }
    let (start, end) = heading_offsets.unwrap_or((0, 0));
    let Some(start) = target.offset_to_position(ctx.documents().position_encoding(), start) else {
        return Ok(None);
    };
    let Some(end) = target.offset_to_position(ctx.documents().position_encoding(), end) else {
        return Ok(None);
    };

    Ok(Some(GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range: Range::new(start, end),
    })))
}

/// Build a Markdown server with the caller's file provider.
pub fn server(file_provider: impl FileProvider) -> Server<State> {
    Server::builder(State)
        .file_provider(file_provider)
        .feature(lspf::features::hover(), hover)
        .feature(
            lspf::features::definition(DefinitionOptions {
                work_done_progress_options: Default::default(),
            }),
            definition,
        )
        .notification::<DidOpenTextDocument, _, _>(did_open)
        .notification::<DidChangeTextDocument, _, _>(did_change)
        .build()
        .expect("valid registrations")
}
