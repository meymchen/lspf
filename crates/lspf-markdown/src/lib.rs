//! First-party Markdown link language server.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use lspf::types::notification::{DidChangeTextDocument, DidOpenTextDocument, PublishDiagnostics};
use lspf::types::{
    DefinitionOptions, Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidOpenTextDocumentParams, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents,
    HoverParams, Location, MarkupContent, MarkupKind, Position, PublishDiagnosticsParams, Range,
    Uri,
};
use lspf::{
    CancellationToken, Document, FileProvider, LspError, PositionEncoding, Server, ServerContext,
};

const SOURCE: &str = "lspf-markdown";

/// Application state for the Markdown server.
pub struct State;

#[derive(Debug)]
struct Link {
    target: String,
    target_start: usize,
    target_end: usize,
}

#[derive(Debug)]
struct ReferenceDefinition {
    target: String,
}

#[derive(Debug)]
struct LocalTarget {
    uri: Uri,
    fragment: Option<String>,
}

impl LocalTarget {
    fn display(&self) -> String {
        self.fragment.as_ref().map_or_else(
            || self.uri.as_str().to_string(),
            |fragment| format!("{}#{fragment}", self.uri.as_str()),
        )
    }
}

#[derive(Debug)]
struct Heading {
    title: String,
    range: Range,
}

#[derive(Debug)]
struct TargetAtPosition {
    source_range: Range,
    local: LocalTarget,
    heading: Option<Heading>,
}

fn normalize_reference_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
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

fn inline_destination(line: &str, start: usize) -> Option<(&str, usize, usize, usize)> {
    if line.as_bytes().get(start) == Some(&b'<') {
        let end = line[start + 1..].find('>')? + start + 1;
        let close = line[end + 1..].find(')')? + end + 1;
        return Some((&line[start + 1..end], start + 1, end, close));
    }

    let mut depth = 0;
    let mut cursor = start;
    let mut target_end = None;
    let bytes = line.as_bytes();
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            b'(' if target_end.is_none() => {
                depth += 1;
                cursor += 1;
            }
            b')' if depth > 0 && target_end.is_none() => {
                depth -= 1;
                cursor += 1;
            }
            b')' if depth == 0 => {
                let end = target_end.unwrap_or(cursor);
                return Some((&line[start..end], start, end, cursor));
            }
            byte if byte.is_ascii_whitespace() && depth == 0 => {
                target_end.get_or_insert(cursor);
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    None
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
            let Some((target, target_start, target_end, destination_end)) =
                inline_destination(line, destination_start)
            else {
                break;
            };
            if !target.is_empty() {
                links.push(Link {
                    target: target.to_string(),
                    target_start: line_start + target_start,
                    target_end: line_start + target_end,
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

fn reference_definitions(text: &str) -> HashMap<String, ReferenceDefinition> {
    let mut definitions = HashMap::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if line.len() - trimmed.len() > 3 || !trimmed.starts_with('[') {
            continue;
        }
        let Some(separator) = trimmed.find("]:") else {
            continue;
        };
        let label = normalize_reference_label(&trimmed[1..separator]);
        let destination = trimmed[separator + 2..].trim_start();
        let target = if let Some(destination) = destination.strip_prefix('<') {
            destination.find('>').map(|end| &destination[..end])
        } else {
            let end = destination
                .find(char::is_whitespace)
                .unwrap_or(destination.len());
            Some(&destination[..end])
        };
        if !label.is_empty()
            && let Some(target) = target.filter(|target| !target.is_empty())
        {
            definitions.insert(
                label,
                ReferenceDefinition {
                    target: target.to_string(),
                },
            );
        }
    }
    definitions
}

fn reference_links(text: &str, definitions: &HashMap<String, ReferenceDefinition>) -> Vec<Link> {
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
        if fenced || trimmed.contains("]:") && trimmed.starts_with('[') {
            line_start += line.len();
            continue;
        }

        let mut cursor = 0;
        while let Some(relative_separator) = line[cursor..].find("][") {
            let separator = cursor + relative_separator;
            if inside_inline_code(line, separator) {
                cursor = separator + 2;
                continue;
            }
            let Some(text_open) = line[..separator].rfind('[') else {
                cursor = separator + 2;
                continue;
            };
            let label_start = separator + 2;
            let Some(relative_close) = line[label_start..].find(']') else {
                break;
            };
            let label_end = label_start + relative_close;
            let (label, range_start, range_end) = if label_start == label_end {
                (&line[text_open + 1..separator], text_open + 1, separator)
            } else {
                (&line[label_start..label_end], label_start, label_end)
            };
            if let Some(definition) = definitions.get(&normalize_reference_label(label)) {
                links.push(Link {
                    target: definition.target.clone(),
                    target_start: line_start + range_start,
                    target_end: line_start + range_end,
                });
            }
            cursor = label_end + 1;
        }
        line_start += line.len();
    }
    links
}

fn markdown_links(text: &str) -> Vec<Link> {
    let definitions = reference_definitions(text);
    let mut links = inline_links(text);
    links.extend(reference_links(text, &definitions));
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

fn resolve_local_target(source: &Uri, target: &str) -> Option<LocalTarget> {
    let (target, fragment) = target
        .split_once('#')
        .map_or((target, None), |(target, fragment)| {
            (target, (!fragment.is_empty()).then(|| fragment.to_string()))
        });
    let target = target.split('?').next().unwrap_or_default();
    if target.is_empty() {
        return Some(LocalTarget {
            uri: source.clone(),
            fragment,
        });
    }
    if target.starts_with("//") {
        return None;
    }
    if has_uri_scheme(target) {
        return target
            .starts_with("file:")
            .then(|| Uri::from_str(target).ok())
            .flatten()
            .map(|uri| LocalTarget { uri, fragment });
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
    Uri::from_str(&normalize_uri_path(&combined))
        .ok()
        .map(|uri| LocalTarget { uri, fragment })
}

fn heading_slug(title: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in title.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '_' || character == '-' {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character);
        } else if character.is_whitespace() {
            pending_dash = true;
        }
    }
    slug
}

fn headings(document: &Document, encoding: PositionEncoding) -> Vec<(String, Heading)> {
    let text = document.text();
    let mut headings = Vec::new();
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        let leading = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();
        let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
        if hashes == 0 || hashes > 6 || !trimmed[hashes..].starts_with(char::is_whitespace) {
            line_start += line.len();
            continue;
        }
        let after_hashes = &trimmed[hashes..];
        let spacing = after_hashes.len() - after_hashes.trim_start().len();
        let title = after_hashes.trim().trim_end_matches('#').trim_end();
        if !title.is_empty() {
            let start_offset = line_start + leading + hashes + spacing;
            let end_offset = start_offset + title.len();
            if let (Some(start), Some(end)) = (
                document.offset_to_position(encoding, start_offset),
                document.offset_to_position(encoding, end_offset),
            ) {
                headings.push((
                    heading_slug(title),
                    Heading {
                        title: title.to_string(),
                        range: Range::new(start, end),
                    },
                ));
            }
        }
        line_start += line.len();
    }
    headings
}

fn selected_heading(
    document: &Document,
    encoding: PositionEncoding,
    fragment: Option<&str>,
) -> Option<Heading> {
    let headings = headings(document, encoding);
    match fragment {
        Some(fragment) => {
            let fragment = fragment.to_lowercase();
            headings
                .into_iter()
                .find_map(|(slug, heading)| (slug == fragment).then_some(heading))
        }
        None => headings.into_iter().next().map(|(_, heading)| heading),
    }
}

async fn target_at_position(
    ctx: &ServerContext,
    uri: &Uri,
    position: Position,
) -> Option<TargetAtPosition> {
    let document = ctx.documents().get(uri)?;
    let encoding = ctx.documents().position_encoding();
    let offset = document.position_to_offset(encoding, position)?;
    let link = markdown_links(&document.text())
        .into_iter()
        .find(|link| link.target_start <= offset && offset <= link.target_end)?;
    let local = resolve_local_target(uri, &link.target)?;
    let target = ctx.workspace().text_document(&local.uri).await.ok()?;
    let heading = selected_heading(&target, encoding, local.fragment.as_deref());
    let source_range = Range::new(
        document.offset_to_position(encoding, link.target_start)?,
        document.offset_to_position(encoding, link.target_end)?,
    );
    Some(TargetAtPosition {
        source_range,
        local,
        heading,
    })
}

async fn publish_diagnostics(ctx: ServerContext, uri: Uri) {
    let Some(document) = ctx.documents().get(&uri) else {
        return;
    };
    let text = document.text();
    let mut diagnostics = Vec::new();
    for link in markdown_links(&text) {
        let Some(local) = resolve_local_target(&uri, &link.target) else {
            continue;
        };
        let target = ctx.workspace().text_document(&local.uri).await;
        let missing_heading = target.as_ref().is_ok_and(|target| {
            local.fragment.is_some()
                && selected_heading(
                    target,
                    ctx.documents().position_encoding(),
                    local.fragment.as_deref(),
                )
                .is_none()
        });
        if target.is_ok() && !missing_heading {
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
            message: if missing_heading {
                format!("local link heading does not exist: {}", link.target)
            } else {
                format!("local link target does not exist: {}", link.target)
            },
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
    let Some(target) =
        target_at_position(&ctx, &uri, params.text_document_position_params.position).await
    else {
        return Ok(None);
    };
    let heading = target
        .heading
        .as_ref()
        .map_or_else(|| target.local.display(), |heading| heading.title.clone());

    Ok(Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("**{heading}**\n\n`{}`", target.local.display()),
        }),
        range: Some(target.source_range),
    }))
}

async fn definition(
    _state: Arc<State>,
    ctx: ServerContext,
    params: GotoDefinitionParams,
    _ct: CancellationToken,
) -> Result<Option<GotoDefinitionResponse>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let Some(target) =
        target_at_position(&ctx, &uri, params.text_document_position_params.position).await
    else {
        return Ok(None);
    };

    Ok(Some(GotoDefinitionResponse::Scalar(Location {
        uri: target.local.uri,
        range: target.heading.map_or_else(
            || Range::new(Position::new(0, 0), Position::new(0, 0)),
            |h| h.range,
        ),
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
