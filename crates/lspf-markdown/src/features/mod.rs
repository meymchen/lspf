//! Language features, one module per LSP capability family.

pub(crate) mod code_actions;
pub(crate) mod completion;
pub(crate) mod definition;
pub(crate) mod diagnostics;
pub(crate) mod file_rename;
pub(crate) mod folding;
pub(crate) mod hover;
pub(crate) mod links;
pub(crate) mod references;
pub(crate) mod rename;
pub(crate) mod selection;
pub(crate) mod symbols;

use std::ops::Range;

use crate::index::Entry;
use crate::parse::{Heading, LinkDef, MdLink, MdReference};

/// A written link destination: an inline href or a definition's destination.
#[derive(Debug, Clone)]
pub(crate) struct Href {
    pub(crate) text: String,
    pub(crate) range: Range<usize>,
}

impl Href {
    /// The path part, before any fragment or query.
    pub(crate) fn path_range(&self) -> Range<usize> {
        let end = self.text.find(['#', '?']).unwrap_or(self.text.len());
        self.range.start..self.range.start + end
    }

    /// The fragment part after `#`, when present.
    pub(crate) fn fragment_range(&self) -> Option<Range<usize>> {
        let hash = self.text.find('#')?;
        Some(self.range.start + hash + 1..self.range.end)
    }
}

/// Every written destination in a document: inline links, autolinks, and
/// definition destinations.
pub(crate) fn hrefs(entry: &Entry) -> impl Iterator<Item = Href> + '_ {
    let links = entry.md.links.iter().map(|link| Href {
        text: link.href.clone(),
        range: link.href_range.clone(),
    });
    let definitions = entry.md.definitions.iter().map(|definition| Href {
        text: definition.dest.clone(),
        range: definition.dest_range.clone(),
    });
    links.chain(definitions)
}

/// What sits under a cursor.
pub(crate) enum Located<'a> {
    Heading(&'a Heading),
    Link(&'a MdLink),
    Reference(&'a MdReference),
    DefinitionLabel(&'a LinkDef),
    DefinitionDest(&'a LinkDef),
}

fn contains(range: &Range<usize>, offset: usize) -> bool {
    range.start <= offset && offset <= range.end
}

pub(crate) fn locate(entry: &Entry, offset: usize) -> Option<Located<'_>> {
    let md = &entry.md;
    if let Some(link) = md
        .links
        .iter()
        .find(|link| contains(&link.href_range, offset))
    {
        return Some(Located::Link(link));
    }
    if let Some(reference) = md
        .references
        .iter()
        .find(|reference| contains(&reference.label_range, offset))
    {
        return Some(Located::Reference(reference));
    }
    if let Some(definition) = md
        .definitions
        .iter()
        .find(|definition| contains(&definition.label_range, offset))
    {
        return Some(Located::DefinitionLabel(definition));
    }
    if let Some(definition) = md
        .definitions
        .iter()
        .find(|definition| contains(&definition.dest_range, offset))
    {
        return Some(Located::DefinitionDest(definition));
    }
    md.headings
        .iter()
        .find(|heading| contains(&heading.range, offset))
        .map(Located::Heading)
}

/// The written destination a located link, reference, or definition names.
pub(crate) fn located_href(entry: &Entry, located: &Located<'_>) -> Option<Href> {
    match located {
        Located::Link(link) => Some(Href {
            text: link.href.clone(),
            range: link.href_range.clone(),
        }),
        Located::Reference(reference) => {
            let definition = entry.md.definition(&reference.label)?;
            Some(Href {
                text: definition.dest.clone(),
                range: reference.label_range.clone(),
            })
        }
        Located::DefinitionDest(definition) => Some(Href {
            text: definition.dest.clone(),
            range: definition.dest_range.clone(),
        }),
        Located::Heading(_) | Located::DefinitionLabel(_) => None,
    }
}
