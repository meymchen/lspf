//! Completion of link paths, heading fragments, and reference labels.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use lspf::types::{
    CompletionItem, CompletionItemKind, CompletionItemTextEdit, CompletionParams,
    CompletionResponse, Position, TextEdit,
};
use lspf::{CancellationToken, LspError, ServerContext};

use crate::State;
use crate::fs::FileKind;
use crate::index::{Entry, WorkspaceIndex};
use crate::link_resolution::LinkResolution;
use crate::parse::{BlockKind, normalize_label};
use crate::target::{encode_path, resolve_local_target};

/// What the text before the cursor asks to complete.
#[derive(Debug, PartialEq, Eq)]
enum Request<'a> {
    /// A destination after `](` or `]:`, and whether it is in `<…>`.
    Path { typed: &'a str, angle: bool },
    /// A label after `[text][`.
    Label { typed: &'a str },
}

fn destination(rest: &str) -> Option<(&str, bool)> {
    match rest.strip_prefix('<') {
        Some(inner) => (!inner.contains('>')).then_some((inner, true)),
        None => (!rest.contains(|c: char| c.is_whitespace() || c == ')')).then_some((rest, false)),
    }
}

fn request(prefix: &str) -> Option<Request<'_>> {
    if let Some(open) = prefix.rfind("](")
        && let Some((typed, angle)) = destination(&prefix[open + 2..])
    {
        return Some(Request::Path { typed, angle });
    }
    let trimmed = prefix.trim_start();
    if trimmed.starts_with('[')
        && let Some(separator) = trimmed.find("]:")
        && let Some((typed, angle)) = destination(trimmed[separator + 2..].trim_start())
    {
        return Some(Request::Path { typed, angle });
    }
    let open = prefix.rfind("][")?;
    let typed = &prefix[open + 2..];
    (prefix[..open].contains('[') && !typed.contains(['[', ']']))
        .then_some(Request::Label { typed })
}

fn item(
    entry: &Entry,
    replace: &Range<usize>,
    label: String,
    new_text: String,
    kind: CompletionItemKind,
    detail: Option<String>,
) -> Option<CompletionItem> {
    Some(CompletionItem {
        filter_text: Some(new_text.clone()),
        text_edit: Some(CompletionItemTextEdit::TextEdit(TextEdit {
            range: entry.range(replace)?,
            new_text,
        })),
        label,
        kind: Some(kind),
        detail,
        ..CompletionItem::default()
    })
}

fn heading_items(
    target: &Entry,
    edit_in: &Entry,
    replace: &Range<usize>,
    prefix: &str,
) -> Vec<CompletionItem> {
    target
        .md
        .headings
        .iter()
        .filter_map(|heading| {
            item(
                edit_in,
                replace,
                format!("#{}", heading.slug),
                format!("{prefix}{}", heading.slug),
                CompletionItemKind::Reference,
                Some(heading.title.clone()),
            )
        })
        .collect()
}

async fn path_items(
    state: &State,
    ctx: &ServerContext,
    entry: &Entry,
    offset: usize,
    typed: &str,
    angle: bool,
) -> Vec<CompletionItem> {
    if let Some((path, fragment)) = typed.split_once('#') {
        let replace = offset - fragment.len()..offset;
        if path.is_empty() {
            return heading_items(entry, entry, &replace, "");
        }
        let Some(resolved) = LinkResolution::new(&state.index, ctx)
            .resolve(entry.uri(), path)
            .await
        else {
            return Vec::new();
        };
        let Some(target) = state.index.get(ctx, &resolved.target.uri).await else {
            return Vec::new();
        };
        return heading_items(&target, entry, &replace, "");
    }

    let (directory, partial) = match typed.rfind('/') {
        Some(slash) => (&typed[..=slash], &typed[slash + 1..]),
        None => ("", typed),
    };
    let replace = offset - partial.len()..offset;
    let root = WorkspaceIndex::root_for(ctx, entry.uri());
    let directory = if directory.is_empty() {
        "./"
    } else {
        directory
    };
    let Some(directory) = resolve_local_target(entry.uri(), directory, root.as_ref()) else {
        return Vec::new();
    };
    let mut items: Vec<CompletionItem> = state
        .index
        .fs()
        .read_directory(&directory.uri)
        .await
        .into_iter()
        .filter(|(name, _)| !name.starts_with('.'))
        .filter_map(|(name, kind)| {
            let written = if angle {
                name.clone()
            } else {
                encode_path(&name)
            };
            let (label, new_text, kind) = match kind {
                FileKind::Directory => (
                    format!("{name}/"),
                    format!("{written}/"),
                    CompletionItemKind::Folder,
                ),
                FileKind::File => (name, written, CompletionItemKind::File),
            };
            item(entry, &replace, label, new_text, kind, None)
        })
        .collect();
    if typed.is_empty() {
        items.extend(heading_items(entry, entry, &replace, "#"));
    }
    items
}

fn label_items(entry: &Entry, offset: usize, typed: &str) -> Vec<CompletionItem> {
    let replace = offset - typed.len()..offset;
    let mut seen = HashSet::new();
    entry
        .md
        .definitions
        .iter()
        .filter(|definition| seen.insert(normalize_label(&definition.label)))
        .filter_map(|definition| {
            item(
                entry,
                &replace,
                definition.label.clone(),
                definition.label.clone(),
                CompletionItemKind::Reference,
                Some(definition.dest.clone()),
            )
        })
        .collect()
}

pub(crate) async fn completion(
    state: Arc<State>,
    ctx: ServerContext,
    params: CompletionParams,
    _ct: CancellationToken,
) -> Result<Option<CompletionResponse>, LspError> {
    let position = params.text_document_position_params;
    let Some(entry) = state.index.get(&ctx, &position.text_document.uri).await else {
        return Ok(None);
    };
    let (Some(offset), Some(line_start)) = (
        entry.offset(position.position),
        entry.offset(Position::new(position.position.line, 0)),
    ) else {
        return Ok(None);
    };
    let in_code = entry.md.blocks.iter().any(|block| {
        matches!(block.kind, BlockKind::CodeBlock | BlockKind::InlineCode)
            && block.range.start < offset
            && offset < block.range.end
    });
    if in_code {
        return Ok(None);
    }
    let text = entry.text();
    let items = match request(&text[line_start..offset]) {
        Some(Request::Path { typed, angle }) => {
            path_items(&state, &ctx, &entry, offset, typed, angle).await
        }
        Some(Request::Label { typed }) => label_items(&entry, offset, typed),
        None => return Ok(None),
    };
    Ok(Some(CompletionResponse::CompletionItemList(items)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_select_the_completion_kind() {
        assert_eq!(
            request("See [a](docs/gu"),
            Some(Request::Path {
                typed: "docs/gu",
                angle: false
            })
        );
        assert_eq!(
            request("See [a](<my do"),
            Some(Request::Path {
                typed: "my do",
                angle: true
            })
        );
        assert_eq!(
            request("[docs]: ../gu"),
            Some(Request::Path {
                typed: "../gu",
                angle: false
            })
        );
        assert_eq!(
            request("[a](x.md#ins"),
            Some(Request::Path {
                typed: "x.md#ins",
                angle: false
            })
        );
        assert_eq!(request("[text][do"), Some(Request::Label { typed: "do" }));
        assert_eq!(request("[a](done.md) and more"), None);
        assert_eq!(
            request("[a](done.md) then [b][la"),
            Some(Request::Label { typed: "la" })
        );
        assert_eq!(request("[a](x.md \"title"), None);
        assert_eq!(request("plain text"), None);
    }
}
