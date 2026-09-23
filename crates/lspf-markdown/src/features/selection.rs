//! Smart selection: nested ranges from the innermost inline span outward to
//! heading sections and the whole document.

use std::ops::Range;
use std::sync::Arc;

use lspf::types::{SelectionRange, SelectionRangeParams};
use lspf::{CancellationToken, LspError, ServerContext};

use crate::State;
use crate::features::symbols::section_range;
use crate::index::Entry;

/// Every span containing `offset`, outermost first.
fn spans(entry: &Entry, offset: usize) -> Vec<Range<usize>> {
    let md = &entry.md;
    let contains = |range: &Range<usize>| range.start <= offset && offset <= range.end;
    let document_end = entry.document.text(None).trim_end().len().max(offset);
    let mut spans: Vec<Range<usize>> = Vec::new();
    spans.push(0..document_end);
    for heading in &md.headings {
        spans.push(section_range(entry, heading));
        spans.push(heading.range.clone());
        spans.push(heading.content.clone());
    }
    spans.extend(md.blocks.iter().map(|block| block.range.clone()));
    spans.extend(md.links.iter().map(|link| link.href_range.clone()));
    spans.extend(
        md.references
            .iter()
            .map(|reference| reference.label_range.clone()),
    );
    for definition in &md.definitions {
        spans.push(definition.range.clone());
        spans.push(definition.label_range.clone());
        spans.push(definition.dest_range.clone());
    }
    spans.retain(contains);
    spans.sort_by(|a, b| {
        (b.end - b.start)
            .cmp(&(a.end - a.start))
            .then(a.start.cmp(&b.start))
    });
    // Each selection must contain the next; drop spans that only overlap,
    // such as a section that starts inside a block quote.
    let mut nested: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for span in spans {
        let fits = nested.last().is_none_or(|outer| {
            outer.start <= span.start && span.end <= outer.end && *outer != span
        });
        if fits {
            nested.push(span);
        }
    }
    nested
}

fn selection(entry: &Entry, offset: usize) -> Option<SelectionRange> {
    let mut current: Option<SelectionRange> = None;
    for span in spans(entry, offset) {
        let range = entry.range(&span)?;
        current = Some(SelectionRange {
            range,
            parent: current.map(Box::new),
        });
    }
    current
}

pub(crate) async fn selection_ranges(
    state: Arc<State>,
    ctx: ServerContext,
    params: SelectionRangeParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<SelectionRange>>, LspError> {
    let Some(entry) = state.index.get(&ctx, &params.text_document.uri).await else {
        return Ok(None);
    };
    let mut ranges = Vec::with_capacity(params.positions.len());
    for position in params.positions {
        let selected = entry
            .offset(position)
            .and_then(|offset| selection(&entry, offset))
            .unwrap_or(SelectionRange {
                range: lspf::types::Range::new(position, position),
                parent: None,
            });
        ranges.push(selected);
    }
    Ok(Some(ranges))
}
