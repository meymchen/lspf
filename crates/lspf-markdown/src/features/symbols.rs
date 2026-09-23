//! Document and workspace symbols: the heading outline.

use std::sync::Arc;

use lspf::types::{
    BaseSymbolInformation, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, Location,
    SymbolKind, WorkspaceSymbol, WorkspaceSymbolLocation, WorkspaceSymbolParams,
    WorkspaceSymbolResponse,
};
use lspf::{CancellationToken, LspError, ServerContext};

use crate::State;
use crate::index::Entry;
use crate::parse::Heading;

fn symbol_name(heading: &Heading) -> String {
    format!(
        "{} {}",
        "#".repeat(usize::from(heading.level)),
        heading.title
    )
}

/// The byte range of a heading's whole section, without trailing blank space.
pub(crate) fn section_range(entry: &Entry, heading: &Heading) -> std::ops::Range<usize> {
    let text = entry.document.text(None);
    let end = heading.section_end.min(text.len());
    let trimmed = text[heading.range.start..end].trim_end().len();
    heading.range.start..heading.range.start + trimmed.max(heading.range.len())
}

fn outline(entry: &Entry) -> Vec<DocumentSymbol> {
    struct Node {
        level: u8,
        symbol: DocumentSymbol,
    }
    fn attach(stack: &mut Vec<Node>, roots: &mut Vec<DocumentSymbol>) {
        let node = stack.pop().expect("attach is called with a node");
        match stack.last_mut() {
            Some(parent) => parent
                .symbol
                .children
                .get_or_insert_with(Vec::new)
                .push(node.symbol),
            None => roots.push(node.symbol),
        }
    }

    let mut roots = Vec::new();
    let mut stack: Vec<Node> = Vec::new();
    for heading in &entry.md.headings {
        let (Some(range), Some(selection_range)) = (
            entry.range(&section_range(entry, heading)),
            entry.range(&heading.range),
        ) else {
            continue;
        };
        while stack.last().is_some_and(|node| node.level >= heading.level) {
            attach(&mut stack, &mut roots);
        }
        #[allow(deprecated)]
        let symbol = DocumentSymbol {
            name: symbol_name(heading),
            detail: None,
            kind: SymbolKind::String,
            tags: None,
            deprecated: None,
            range,
            selection_range,
            children: None,
        };
        stack.push(Node {
            level: heading.level,
            symbol,
        });
    }
    while !stack.is_empty() {
        attach(&mut stack, &mut roots);
    }
    roots
}

pub(crate) async fn document_symbols(
    state: Arc<State>,
    ctx: ServerContext,
    params: DocumentSymbolParams,
    _ct: CancellationToken,
) -> Result<Option<DocumentSymbolResponse>, LspError> {
    let Some(entry) = state.index.get(&ctx, &params.text_document.uri).await else {
        return Ok(None);
    };
    Ok(Some(DocumentSymbolResponse::DocumentSymbolList(outline(
        &entry,
    ))))
}

/// Whether every character of `query` appears in `name`, in order and
/// ignoring case.
fn matches_query(name: &str, query: &str) -> bool {
    let mut name = name.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| !character.is_whitespace())
        .all(|wanted| name.any(|character| character == wanted))
}

pub(crate) async fn workspace_symbols(
    state: Arc<State>,
    ctx: ServerContext,
    params: WorkspaceSymbolParams,
    ct: CancellationToken,
) -> Result<Option<WorkspaceSymbolResponse>, LspError> {
    let mut symbols = Vec::new();
    for entry in state.index.all(&ctx).await {
        if ct.is_cancelled() {
            return Err(LspError::RequestCancelled);
        }
        for heading in &entry.md.headings {
            let name = symbol_name(heading);
            if !matches_query(&name, &params.query) {
                continue;
            }
            let Some(range) = entry.range(&heading.range) else {
                continue;
            };
            symbols.push(WorkspaceSymbol {
                location: WorkspaceSymbolLocation::Location(Location {
                    uri: entry.uri().clone(),
                    range,
                }),
                data: None,
                base_symbol_information: BaseSymbolInformation {
                    name,
                    kind: SymbolKind::String,
                    tags: None,
                    container_name: None,
                },
            });
        }
    }
    Ok(Some(WorkspaceSymbolResponse::WorkspaceSymbolList(symbols)))
}

#[cfg(test)]
mod tests {
    use super::matches_query;

    #[test]
    fn queries_match_ordered_subsequences() {
        assert!(matches_query("## Getting Started", "gs"));
        assert!(matches_query("## Getting Started", "START"));
        assert!(matches_query("## Getting Started", ""));
        assert!(!matches_query("## Getting Started", "sg x"));
    }
}
