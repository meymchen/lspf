//! Folding ranges for heading sections, regions, and multi-line blocks.

use std::collections::HashSet;
use std::sync::Arc;

use lspf::types::{FoldingRange, FoldingRangeKind, FoldingRangeParams};
use lspf::{CancellationToken, LspError, ServerContext};

use crate::State;
use crate::features::symbols::section_range;
use crate::index::Entry;
use crate::parse::BlockKind;

fn fold(
    entry: &Entry,
    range: &std::ops::Range<usize>,
    kind: Option<FoldingRangeKind>,
) -> Option<FoldingRange> {
    let start = entry.position(range.start)?.line;
    let end = entry.position(range.end)?.line;
    (end > start).then(|| FoldingRange {
        start_line: start,
        end_line: end,
        kind,
        ..FoldingRange::default()
    })
}

pub(crate) fn folding_ranges(entry: &Entry) -> Vec<FoldingRange> {
    let text = entry.document.text(None);
    let md = &entry.md;
    let mut ranges = Vec::new();
    for heading in &md.headings {
        ranges.extend(fold(entry, &section_range(entry, heading), None));
    }
    for region in &md.regions {
        ranges.extend(fold(entry, region, Some(FoldingRangeKind::Region)));
    }
    for block in &md.blocks {
        let kind = match block.kind {
            BlockKind::HtmlBlock => {
                let marker = md.regions.iter().any(|region| {
                    region.start == block.range.start || region.end == block.range.end
                });
                if marker {
                    continue;
                }
                text[block.range.clone()]
                    .trim_start()
                    .starts_with("<!--")
                    .then_some(FoldingRangeKind::Comment)
            }
            BlockKind::CodeBlock
            | BlockKind::Table
            | BlockKind::BlockQuote
            | BlockKind::List
            | BlockKind::Item
            | BlockKind::Metadata
            | BlockKind::Footnote => None,
            _ => continue,
        };
        ranges.extend(fold(entry, &block.range, kind));
    }
    let mut seen = HashSet::new();
    ranges.retain(|range| seen.insert((range.start_line, range.end_line)));
    ranges.sort_by_key(|range| (range.start_line, std::cmp::Reverse(range.end_line)));
    ranges
}

pub(crate) async fn folding(
    state: Arc<State>,
    ctx: ServerContext,
    params: FoldingRangeParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<FoldingRange>>, LspError> {
    Ok(state
        .index
        .get(&ctx, &params.text_document.uri)
        .await
        .map(|entry| folding_ranges(&entry)))
}
