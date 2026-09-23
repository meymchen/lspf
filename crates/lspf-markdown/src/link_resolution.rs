//! One interpretation of Markdown links for navigation, occurrences, and edits.
//!
//! A value lives for one Handler invocation. Both successful and missing
//! existence checks are retained only for that invocation, so nested searches
//! agree without adding a persistent cache or promising a filesystem snapshot.

use std::collections::HashMap;
use std::ops::Range;
use std::str::FromStr;
use std::sync::Arc;

use lspf::ServerContext;
use lspf::types::{Location, Uri};

use crate::fs::FileKind;
use crate::index::{Entry, WorkspaceIndex};
use crate::parse::{Located, hrefs, locate, located_href, normalize_label};
use crate::slug::fragment_matches;
use crate::target::{
    LocalTarget, encode_path, file_name, is_external, percent_decode, relative_path,
    resolve_local_target, root_relative_path, uri_key, without_fragment,
};

#[derive(Debug, Clone)]
pub(crate) struct ResolvedTarget {
    pub(crate) target: LocalTarget,
    pub(crate) kind: Option<FileKind>,
}

pub(crate) enum Symbol {
    Heading { uri: Uri, slug: String },
    File { target: ResolvedTarget },
    Label { label: String },
}

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

pub(crate) enum Search {
    CursorOnly,
    Document,
    Workspace,
    /// Heading and label edits need occurrences; resource moves do their own scan.
    Rename,
}

pub(crate) struct Selection {
    pub(crate) symbol: Symbol,
    pub(crate) range: Range<usize>,
    pub(crate) occurrences: Vec<Occurrence>,
}

pub(crate) struct LinkEdit {
    pub(crate) entry: Arc<Entry>,
    pub(crate) range: Range<usize>,
    pub(crate) new_text: String,
}

pub(crate) struct LinkResolution<'a> {
    index: &'a WorkspaceIndex,
    ctx: &'a ServerContext,
    presence: HashMap<String, Option<FileKind>>,
}

impl<'a> LinkResolution<'a> {
    pub(crate) fn new(index: &'a WorkspaceIndex, ctx: &'a ServerContext) -> Self {
        Self {
            index,
            ctx,
            presence: HashMap::new(),
        }
    }

    /// Select an exact resource first, then an existing `.md` fallback. A
    /// missing target retains the written resource's exact URI.
    pub(crate) async fn resolve(&mut self, source: &Uri, href: &str) -> Option<ResolvedTarget> {
        self.resolve_after_moves(source, href, &[]).await
    }

    async fn resolve_after_moves(
        &mut self,
        source: &Uri,
        href: &str,
        moves: &[(Uri, Uri)],
    ) -> Option<ResolvedTarget> {
        if is_external(href) {
            return None;
        }
        let root = WorkspaceIndex::root_for(self.ctx, source);
        let target = resolve_local_target(source, href, root.as_ref())?;
        let kind = self.stat_after_moves(&target.uri, moves).await;
        if kind.is_none() && !file_name(&target.uri).contains('.') {
            let with_extension = LocalTarget {
                uri: format!("{}.md", target.uri.as_str()).parse().ok()?,
                fragment: target.fragment.clone(),
            };
            if let Some(kind) = self.stat_after_moves(&with_extension.uri, moves).await {
                return Some(ResolvedTarget {
                    target: with_extension,
                    kind: Some(kind),
                });
            }
        }
        Some(ResolvedTarget { target, kind })
    }

    async fn stat(&mut self, uri: &Uri) -> Option<FileKind> {
        let key = uri_key(uri);
        if let Some(kind) = self.presence.get(&key) {
            return *kind;
        }
        let kind = if self.index.is_open(uri) || self.ctx.documents().get(uri).is_some() {
            Some(FileKind::File)
        } else {
            self.index.fs().stat(uri).await
        };
        self.presence.insert(key, kind);
        kind
    }

    /// Inspect the projected resource by reading its original location. Incoming
    /// moves take precedence over vacated locations, including swaps. Every real
    /// existence check still goes through the invocation's presence memo.
    async fn stat_after_moves(&mut self, uri: &Uri, moves: &[(Uri, Uri)]) -> Option<FileKind> {
        for (old, new) in moves {
            if let Some(original) = relocated(uri, new, old) {
                return self.stat(&original).await;
            }
        }
        // A successful move also places its resource beneath its new ancestors.
        let prefix = format!("{}/", uri_key(uri));
        for (old, new) in moves {
            if uri_key(new).starts_with(&prefix) && self.stat(old).await.is_some() {
                return Some(FileKind::Directory);
            }
        }
        if moved(uri, moves).is_some() {
            return None;
        }
        self.stat(uri).await
    }

    /// Interpret the cursor once, including its replacement range, and find
    /// occurrences in the requested scope. Labels always stay in this Document.
    pub(crate) async fn select(
        &mut self,
        entry: Arc<Entry>,
        offset: usize,
        search: Search,
    ) -> Option<Selection> {
        let (symbol, range) = self.symbol_at(&entry, offset).await?;
        let entries = match (&search, &symbol) {
            (Search::CursorOnly, _) | (Search::Rename, Symbol::File { .. }) => Vec::new(),
            (_, Symbol::Label { .. }) | (Search::Document, _) => vec![entry],
            (Search::Workspace | Search::Rename, _) => self.index.all(self.ctx).await,
        };
        let occurrences = if matches!(search, Search::CursorOnly)
            || matches!((&search, &symbol), (Search::Rename, Symbol::File { .. }))
        {
            Vec::new()
        } else {
            self.occurrences(&symbol, &entries, matches!(search, Search::Document))
                .await
        };
        Some(Selection {
            symbol,
            range,
            occurrences,
        })
    }

    async fn symbol_at(&mut self, entry: &Entry, offset: usize) -> Option<(Symbol, Range<usize>)> {
        let located = locate(&entry.md, offset)?;
        match &located {
            Located::Heading(heading) => {
                return Some((
                    Symbol::Heading {
                        uri: entry.uri().clone(),
                        slug: heading.slug.clone(),
                    },
                    heading.content.clone(),
                ));
            }
            Located::Reference(reference) => {
                return Some((
                    Symbol::Label {
                        label: normalize_label(&reference.label),
                    },
                    reference.label_range.clone(),
                ));
            }
            Located::DefinitionLabel(definition) => {
                return Some((
                    Symbol::Label {
                        label: normalize_label(&definition.label),
                    },
                    definition.label_range.clone(),
                ));
            }
            Located::Link(_) | Located::DefinitionDest(_) => {}
        }
        let href = located_href(&entry.md, &located)?;
        let resolved = self.resolve(entry.uri(), &href.text).await?;
        let fragment_range = href.fragment_range();
        if href.path_range().is_empty()
            || fragment_range
                .as_ref()
                .is_some_and(|range| offset >= range.start)
        {
            let fragment_range = fragment_range?;
            let fragment = resolved.target.fragment.as_deref()?;
            let target = self.index.get(self.ctx, &resolved.target.uri).await?;
            let heading = target.md.heading_for_fragment(fragment)?;
            return Some((
                Symbol::Heading {
                    uri: resolved.target.uri,
                    slug: heading.slug.clone(),
                },
                fragment_range,
            ));
        }
        Some((Symbol::File { target: resolved }, href.path_range()))
    }

    async fn occurrences(
        &mut self,
        symbol: &Symbol,
        entries: &[Arc<Entry>],
        current_document_only: bool,
    ) -> Vec<Occurrence> {
        let mut found = Vec::new();
        match symbol {
            Symbol::Label { label } => {
                for entry in entries {
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
                if let Some(target) = self.index.get(self.ctx, uri).await
                    && (!current_document_only
                        || entries.iter().any(|entry| entry.uri() == target.uri()))
                    && let Some(heading) = target.md.headings.iter().find(|h| h.slug == *slug)
                {
                    found.push(Occurrence {
                        entry: target.clone(),
                        range: heading.content.clone(),
                        declaration: true,
                    });
                }
                for entry in entries {
                    for href in hrefs(&entry.md) {
                        let Some(range) = href.fragment_range() else {
                            continue;
                        };
                        let Some(resolved) = self.resolve(entry.uri(), &href.text).await else {
                            continue;
                        };
                        if uri_key(&resolved.target.uri) == uri_key(uri)
                            && resolved
                                .target
                                .fragment
                                .as_deref()
                                .is_some_and(|fragment| fragment_matches(fragment, slug))
                        {
                            found.push(Occurrence {
                                entry: Arc::clone(entry),
                                range,
                                declaration: false,
                            });
                        }
                    }
                }
            }
            Symbol::File { target } => {
                for entry in entries {
                    for href in hrefs(&entry.md) {
                        let range = href.path_range();
                        if range.is_empty() {
                            continue;
                        }
                        if let Some(resolved) = self.resolve(entry.uri(), &href.text).await
                            && uri_key(&resolved.target.uri) == uri_key(&target.target.uri)
                        {
                            found.push(Occurrence {
                                entry: Arc::clone(entry),
                                range,
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

    /// Plan replacements against the supplied moves without applying them.
    /// Keep the written path whenever it still selects the intended resource.
    pub(crate) async fn edits_for_moves(&mut self, moves: &[(Uri, Uri)]) -> Vec<LinkEdit> {
        let mut edits = Vec::new();
        for entry in self.index.all(self.ctx).await {
            let source = entry.uri();
            let new_source = moved(source, moves).unwrap_or_else(|| source.clone());
            let root = WorkspaceIndex::root_for(self.ctx, &new_source);
            for href in hrefs(&entry.md) {
                let range = href.path_range();
                if range.is_empty() {
                    continue;
                }
                let Some(resolved) = self.resolve(source, &href.text).await else {
                    continue;
                };
                let actual = moved(&resolved.target.uri, moves).unwrap_or(resolved.target.uri);
                let actual_key = uri_key(&actual);
                if self
                    .resolve_after_moves(&new_source, &href.text, moves)
                    .await
                    .is_some_and(|after| uri_key(&after.target.uri) == actual_key)
                {
                    continue;
                }
                let original = &href.text[..range.len()];
                let mut path = respell(original, &new_source, &actual, root.as_ref());
                // Extension omission is a semantic choice: the short spelling
                // must still select this resource in the projected workspace.
                if !original
                    .rsplit('/')
                    .next()
                    .unwrap_or(original)
                    .contains('.')
                    && let Some(short) = path.strip_suffix(".md")
                    && self
                        .resolve_after_moves(&new_source, short, moves)
                        .await
                        .is_some_and(|after| uri_key(&after.target.uri) == actual_key)
                {
                    path = short.to_string();
                }
                if !self
                    .resolve_after_moves(&new_source, &path, moves)
                    .await
                    .is_some_and(|after| uri_key(&after.target.uri) == actual_key)
                {
                    // A path style can become unrepresentable after a move
                    // across roots or authorities. Preserve the target first.
                    path = actual.as_str().to_string();
                    if !self
                        .resolve_after_moves(&new_source, &path, moves)
                        .await
                        .is_some_and(|after| uri_key(&after.target.uri) == actual_key)
                    {
                        continue;
                    }
                }
                if path != original {
                    edits.push(LinkEdit {
                        entry: Arc::clone(&entry),
                        range,
                        new_text: path,
                    });
                }
            }
        }
        edits
    }
}

/// Relocate an exact resource or one of its directory descendants. These are
/// literal resource identities, so extension fallback never applies here.
fn relocated(uri: &Uri, old: &Uri, new: &Uri) -> Option<Uri> {
    let key = uri_key(uri);
    if key == uri_key(old) {
        return Some(new.clone());
    }
    let rest = key.strip_prefix(&format!("{}/", uri_key(old)))?;
    let base = without_fragment(new.as_str()).trim_end_matches('/');
    Uri::from_str(&format!("{base}/{}", encode_path(rest))).ok()
}

fn moved(uri: &Uri, moves: &[(Uri, Uri)]) -> Option<Uri> {
    moves.iter().find_map(|(old, new)| relocated(uri, old, new))
}

/// Spell the complete target path in the original style. The caller alone can
/// decide whether an extension may be omitted after considering all moves.
fn respell(original: &str, source: &Uri, target: &Uri, root: Option<&Uri>) -> String {
    let mut path = if original.starts_with("file:") {
        target.as_str().to_string()
    } else if original.starts_with('/') {
        root.and_then(|root| root_relative_path(root, target))
            .unwrap_or_else(|| target.as_str().to_string())
    } else {
        let Some(relative) = relative_path(source, target) else {
            return target.as_str().to_string();
        };
        if original.starts_with("./") && !relative.starts_with("..") {
            format!("./{relative}")
        } else {
            relative
        }
    };
    if !original.contains('%') {
        let decoded = percent_decode(&path);
        if !decoded
            .contains(|character: char| character.is_whitespace() || "<>#?%".contains(character))
        {
            path = decoded;
        }
    }
    path
}
