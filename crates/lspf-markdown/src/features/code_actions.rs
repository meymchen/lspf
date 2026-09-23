//! Code actions for link definitions: organize them, extract a link into
//! one, and quick fixes for duplicate or unused definitions.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lspf::types::{
    Code, CodeAction, CodeActionKind, CodeActionParams, CodeActionResponse, Diagnostic, Position,
    Range, TextEdit, WorkspaceEdit,
};
use lspf::{CancellationToken, LspError, ServerContext};

use crate::State;
use crate::features::diagnostics::{DUPLICATE_DEFINITION, UNUSED_DEFINITION};
use crate::index::Entry;
use crate::parse::{LinkDef, normalize_label};

/// The kind of the organize action. Clients request it by name.
pub(crate) const ORGANIZE_LINK_DEFINITIONS: &str = "source.organizeLinkDefinitions";

fn organize_kind() -> CodeActionKind {
    CodeActionKind::Custom(Cow::Borrowed(ORGANIZE_LINK_DEFINITIONS))
}

fn wanted(only: Option<&[CodeActionKind]>, kind: &CodeActionKind) -> bool {
    let Some(only) = only else {
        return true;
    };
    let kind = kind_str(kind);
    only.iter().any(|requested| {
        let requested = kind_str(requested);
        kind == requested || kind.starts_with(&format!("{requested}."))
    })
}

fn kind_str(kind: &CodeActionKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn edit(entry: &Entry, edits: Vec<TextEdit>) -> WorkspaceEdit {
    WorkspaceEdit {
        changes: Some(HashMap::from([(entry.uri().clone(), edits)])),
        ..WorkspaceEdit::default()
    }
}

/// The whole lines a definition occupies, line ending included.
fn line_range(entry: &Entry, definition: &LinkDef) -> Option<Range> {
    let start = entry.position(definition.range.start)?;
    let end = entry.position(definition.range.end)?;
    Some(Range::new(
        Position::new(start.line, 0),
        Position::new(end.line + 1, 0),
    ))
}

fn remove_definition(
    entry: &Entry,
    diagnostic: &Diagnostic,
    title: &str,
) -> Option<CodeActionResponse> {
    let start = entry.offset(diagnostic.range.start)?;
    let definition = entry
        .md
        .definitions
        .iter()
        .find(|definition| definition.range.start <= start && start <= definition.range.end)?;
    Some(CodeActionResponse::CodeAction(CodeAction {
        title: title.to_string(),
        kind: Some(CodeActionKind::QuickFix),
        diagnostics: Some(vec![diagnostic.clone()]),
        is_preferred: Some(true),
        edit: Some(edit(
            entry,
            vec![TextEdit {
                range: line_range(entry, definition)?,
                new_text: String::new(),
            }],
        )),
        ..CodeAction::default()
    }))
}

/// Whether a definition starts its line. Definitions inside block quotes or
/// list items belong to that container and stay where they are.
fn top_level(text: &str, definition: &LinkDef) -> bool {
    let line_start = text[..definition.range.start]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    text[line_start..definition.range.start].trim().is_empty()
}

/// Rewrite the document with every top-level definition grouped at its end,
/// sorted by label, optionally dropping those no reference uses.
fn organized(entry: &Entry, remove_unused: bool) -> Option<String> {
    let text = entry.text();
    let md = &entry.md;
    let movable: Vec<&LinkDef> = md
        .definitions
        .iter()
        .filter(|definition| top_level(&text, definition))
        .collect();
    if movable.is_empty() {
        return None;
    }
    let used: HashSet<String> = md
        .references
        .iter()
        .map(|reference| normalize_label(&reference.label))
        .collect();
    let mut definitions: Vec<&LinkDef> = movable
        .iter()
        .copied()
        .filter(|definition| !remove_unused || used.contains(&normalize_label(&definition.label)))
        .collect();
    definitions.sort_by_key(|definition| normalize_label(&definition.label));

    let mut body = String::with_capacity(text.len());
    let mut cursor = 0;
    for definition in &movable {
        let line_start = text[..definition.range.start]
            .rfind('\n')
            .map_or(0, |newline| newline + 1);
        let line_end = text[definition.range.end..]
            .find('\n')
            .map_or(text.len(), |newline| definition.range.end + newline + 1);
        if line_start < cursor {
            continue;
        }
        body.push_str(&text[cursor..line_start]);
        cursor = line_end;
        // Removing a definition must not leave a run of blank lines behind.
        if body.is_empty() || body.ends_with("\n\n") {
            while let Some(blank) = text[cursor..]
                .split_inclusive('\n')
                .next()
                .filter(|line| line.ends_with('\n') && line.trim().is_empty())
            {
                cursor += blank.len();
            }
        }
    }
    body.push_str(&text[cursor..]);
    let mut organized = body.trim_end().to_string();
    if !definitions.is_empty() {
        if !organized.is_empty() {
            organized.push_str("\n\n");
        }
        let lines: Vec<&str> = definitions
            .iter()
            .map(|definition| text[definition.range.clone()].trim())
            .collect();
        organized.push_str(&lines.join("\n"));
    }
    organized.push('\n');
    (organized != text).then_some(organized)
}

fn organize_action(entry: &Entry, remove_unused: bool) -> Option<CodeActionResponse> {
    let new_text = organized(entry, remove_unused)?;
    let end = entry.position(entry.text().len())?;
    Some(CodeActionResponse::CodeAction(CodeAction {
        title: if remove_unused {
            "Organize link definitions and remove unused ones".to_string()
        } else {
            "Organize link definitions".to_string()
        },
        kind: Some(organize_kind()),
        edit: Some(edit(
            entry,
            vec![TextEdit {
                range: Range::new(Position::new(0, 0), end),
                new_text,
            }],
        )),
        ..CodeAction::default()
    }))
}

fn unique_label(entry: &Entry, href: &str) -> String {
    let path = href.split(['#', '?']).next().unwrap_or(href);
    let stem = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .split('.')
        .next()
        .unwrap_or_default();
    let base = crate::slug::slugify(&crate::target::percent_decode(stem));
    let base = if base.is_empty() {
        "link".to_string()
    } else {
        base
    };
    let taken: HashSet<String> = entry
        .md
        .definitions
        .iter()
        .map(|definition| normalize_label(&definition.label))
        .collect();
    std::iter::once(base.clone())
        .chain((1..).map(|index| format!("{base}-{index}")))
        .find(|label| !taken.contains(label))
        .expect("an unbounded label sequence has a free label")
}

fn extract_action(entry: &Entry, offset: usize) -> Option<CodeActionResponse> {
    let text = entry.text();
    let md = &entry.md;
    let selected = md
        .links
        .iter()
        .find(|link| !link.autolink && link.range.start <= offset && offset <= link.range.end)?;
    let label = unique_label(entry, &selected.href);
    let mut edits = Vec::new();
    for link in md
        .links
        .iter()
        .filter(|link| !link.autolink && link.href == selected.href)
    {
        let close = text[link.range.start..link.href_range.start].rfind("](")? + link.range.start;
        edits.push(TextEdit {
            range: entry.range(&(close + 1..link.range.end))?,
            new_text: format!("[{label}]"),
        });
    }
    let end = entry.position(text.len())?;
    let separator = if text.ends_with("\n\n") {
        ""
    } else if text.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    edits.push(TextEdit {
        range: Range::new(end, end),
        new_text: format!("{separator}[{label}]: {}\n", selected.href),
    });
    Some(CodeActionResponse::CodeAction(CodeAction {
        title: "Extract to link definition".to_string(),
        kind: Some(CodeActionKind::RefactorExtract),
        edit: Some(edit(entry, edits)),
        ..CodeAction::default()
    }))
}

pub(crate) async fn code_actions(
    state: Arc<State>,
    ctx: ServerContext,
    params: CodeActionParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<CodeActionResponse>>, LspError> {
    let Some(entry) = state.index.get(&ctx, &params.text_document.uri).await else {
        return Ok(None);
    };
    let only = params.context.only.as_deref();
    let mut actions = Vec::new();

    if wanted(only, &CodeActionKind::QuickFix) {
        for diagnostic in &params.context.diagnostics {
            let title = match &diagnostic.code {
                Some(Code::String(code)) if code == UNUSED_DEFINITION => {
                    "Remove unused link definition"
                }
                Some(Code::String(code)) if code == DUPLICATE_DEFINITION => {
                    "Remove duplicate link definition"
                }
                _ => continue,
            };
            actions.extend(remove_definition(&entry, diagnostic, title));
        }
    }
    if wanted(only, &organize_kind()) {
        actions.extend(organize_action(&entry, false));
        actions.extend(
            organize_action(&entry, true)
                .filter(|_| organized(&entry, true) != organized(&entry, false)),
        );
    }
    if wanted(only, &CodeActionKind::RefactorExtract)
        && let Some(offset) = entry.offset(params.range.start)
    {
        actions.extend(extract_action(&entry, offset));
    }
    Ok(Some(actions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_kinds_include_their_subkinds() {
        assert!(wanted(None, &CodeActionKind::QuickFix));
        assert!(wanted(Some(&[CodeActionKind::Source]), &organize_kind()));
        assert!(wanted(
            Some(&[CodeActionKind::Refactor]),
            &CodeActionKind::RefactorExtract
        ));
        assert!(!wanted(Some(&[CodeActionKind::QuickFix]), &organize_kind()));
    }
}
