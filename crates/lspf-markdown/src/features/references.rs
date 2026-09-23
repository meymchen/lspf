//! Find references and document highlights over one shared symbol finder.
//!
//! A cursor names one of three symbols: a heading (from the heading itself or
//! a link fragment), a file (from a link path), or a reference label (from a
//! reference or its definition). Occurrences carry the exact range a rename
//! would replace: the heading text, the fragment, the path, or the label.

use std::ops::Range;
use std::sync::Arc;

use lspf::types::{
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams, Location, ReferenceParams,
    Uri,
};
use lspf::{CancellationToken, LspError, ServerContext};

use crate::State;
use crate::features::{Located, hrefs, locate, located_href};
use crate::index::{Entry, MARKDOWN_EXTENSIONS, WorkspaceIndex};
use crate::parse::normalize_label;
use crate::slug::fragment_matches;
use crate::target::{is_external, resolve_local_target, uri_key};

#[derive(Debug, Clone)]
pub(crate) enum Symbol {
    Heading { uri: Uri, slug: String },
    File { uri: Uri },
    Label { uri: Uri, label: String },
}

#[derive(Clone)]
pub(crate) struct Occurrence {
    pub(crate) entry: Arc<Entry>,
    pub(crate) range: Range<usize>,
    pub(crate) declaration: bool,
}

impl Occurrence {
    pub(crate) fn location(&self) -> Option<Location> {
        Some(Location {
            uri: self.entry.uri().clone(),
            range: self.entry.range(&self.range)?,
        })
    }
}

/// Whether a destination resolved without stat names `target`, allowing the
/// extensionless spelling of a Markdown file.
pub(crate) fn names(resolved: &Uri, target: &Uri) -> bool {
    let resolved = uri_key(resolved);
    let target = uri_key(target);
    resolved == target
        || MARKDOWN_EXTENSIONS
            .iter()
            .any(|extension| format!("{resolved}.{extension}") == target)
}

/// The symbol under `offset` in `entry`.
pub(crate) async fn symbol_at(
    state: &State,
    ctx: &ServerContext,
    entry: &Entry,
    offset: usize,
) -> Option<Symbol> {
    let located = locate(entry, offset)?;
    match &located {
        Located::Heading(heading) => {
            return Some(Symbol::Heading {
                uri: entry.uri().clone(),
                slug: heading.slug.clone(),
            });
        }
        Located::Reference(reference) => {
            return Some(Symbol::Label {
                uri: entry.uri().clone(),
                label: normalize_label(&reference.label),
            });
        }
        Located::DefinitionLabel(definition) => {
            return Some(Symbol::Label {
                uri: entry.uri().clone(),
                label: normalize_label(&definition.label),
            });
        }
        Located::Link(_) | Located::DefinitionDest(_) => {}
    }
    let href = located_href(entry, &located)?;
    if is_external(&href.text) {
        return None;
    }
    let resolved = state.index.resolve(ctx, entry.uri(), &href.text).await?;
    let in_fragment = href
        .fragment_range()
        .is_some_and(|fragment| offset >= fragment.start);
    if in_fragment || href.path_range().is_empty() {
        let fragment = resolved.target.fragment.as_deref()?;
        let target = state.index.get(ctx, &resolved.target.uri).await?;
        let heading = target.md.heading_for_fragment(fragment)?;
        return Some(Symbol::Heading {
            uri: resolved.target.uri,
            slug: heading.slug.clone(),
        });
    }
    Some(Symbol::File {
        uri: resolved.target.uri,
    })
}

/// Every occurrence of `symbol` in `entries`, plus its declaration.
pub(crate) async fn occurrences(
    state: &State,
    ctx: &ServerContext,
    symbol: &Symbol,
    entries: &[Arc<Entry>],
) -> Vec<Occurrence> {
    let mut found = Vec::new();
    match symbol {
        Symbol::Label { uri, label } => {
            for entry in entries.iter().filter(|entry| names(entry.uri(), uri)) {
                for definition in &entry.md.definitions {
                    if normalize_label(&definition.label) == *label {
                        found.push(Occurrence {
                            entry: Arc::clone(entry),
                            range: definition.label_range.clone(),
                            declaration: true,
                        });
                    }
                }
                for reference in &entry.md.references {
                    if normalize_label(&reference.label) == *label {
                        found.push(Occurrence {
                            entry: Arc::clone(entry),
                            range: reference.label_range.clone(),
                            declaration: false,
                        });
                    }
                }
            }
        }
        Symbol::Heading { uri, slug } => {
            if let Some(target) = state.index.get(ctx, uri).await
                && let Some(heading) = target.md.headings.iter().find(|h| h.slug == *slug)
            {
                found.push(Occurrence {
                    entry: target.clone(),
                    range: heading.content.clone(),
                    declaration: true,
                });
            }
            for entry in entries {
                let root = WorkspaceIndex::root_for(ctx, entry.uri());
                for href in hrefs(entry) {
                    let (Some(fragment_range), false) =
                        (href.fragment_range(), is_external(&href.text))
                    else {
                        continue;
                    };
                    let Some(target) = resolve_local_target(entry.uri(), &href.text, root.as_ref())
                    else {
                        continue;
                    };
                    if names(&target.uri, uri)
                        && target
                            .fragment
                            .as_deref()
                            .is_some_and(|fragment| fragment_matches(fragment, slug))
                    {
                        found.push(Occurrence {
                            entry: Arc::clone(entry),
                            range: fragment_range,
                            declaration: false,
                        });
                    }
                }
            }
        }
        Symbol::File { uri } => {
            for entry in entries {
                let root = WorkspaceIndex::root_for(ctx, entry.uri());
                for href in hrefs(entry) {
                    let path = href.path_range();
                    if path.is_empty() || is_external(&href.text) {
                        continue;
                    }
                    if resolve_local_target(entry.uri(), &href.text, root.as_ref())
                        .is_some_and(|target| names(&target.uri, uri))
                    {
                        found.push(Occurrence {
                            entry: Arc::clone(entry),
                            range: path,
                            declaration: false,
                        });
                    }
                }
            }
        }
    }
    found.sort_by(|a, b| {
        a.entry
            .uri()
            .as_str()
            .cmp(b.entry.uri().as_str())
            .then(a.range.start.cmp(&b.range.start))
    });
    found
}

/// The entries a symbol's occurrences can live in.
pub(crate) async fn scope(state: &State, ctx: &ServerContext, symbol: &Symbol) -> Vec<Arc<Entry>> {
    match symbol {
        Symbol::Label { uri, .. } => state.index.get(ctx, uri).await.into_iter().collect(),
        Symbol::Heading { .. } | Symbol::File { .. } => state.index.all(ctx).await,
    }
}

pub(crate) async fn references(
    state: Arc<State>,
    ctx: ServerContext,
    params: ReferenceParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<Location>>, LspError> {
    let position = params.text_document_position_params;
    let Some(entry) = state.index.get(&ctx, &position.text_document.uri).await else {
        return Ok(None);
    };
    let Some(offset) = entry.offset(position.position) else {
        return Ok(None);
    };
    let Some(symbol) = symbol_at(&state, &ctx, &entry, offset).await else {
        return Ok(None);
    };
    let entries = scope(&state, &ctx, &symbol).await;
    let locations = occurrences(&state, &ctx, &symbol, &entries)
        .await
        .into_iter()
        .filter(|occurrence| params.context.include_declaration || !occurrence.declaration)
        .filter_map(|occurrence| occurrence.location())
        .collect();
    Ok(Some(locations))
}

pub(crate) async fn document_highlights(
    state: Arc<State>,
    ctx: ServerContext,
    params: DocumentHighlightParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<DocumentHighlight>>, LspError> {
    let position = params.text_document_position_params;
    let Some(entry) = state.index.get(&ctx, &position.text_document.uri).await else {
        return Ok(None);
    };
    let Some(offset) = entry.offset(position.position) else {
        return Ok(None);
    };
    let Some(symbol) = symbol_at(&state, &ctx, &entry, offset).await else {
        return Ok(None);
    };
    let current = uri_key(entry.uri());
    let highlights = occurrences(&state, &ctx, &symbol, &[Arc::clone(&entry)])
        .await
        .into_iter()
        .filter(|occurrence| uri_key(occurrence.entry.uri()) == current)
        .filter_map(|occurrence| {
            Some(DocumentHighlight {
                range: occurrence.entry.range(&occurrence.range)?,
                kind: Some(if occurrence.declaration {
                    DocumentHighlightKind::Write
                } else {
                    DocumentHighlightKind::Read
                }),
            })
        })
        .collect();
    Ok(Some(highlights))
}
