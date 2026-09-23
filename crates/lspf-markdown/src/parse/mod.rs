//! The Markdown document model every feature reads.
//!
//! [`MdDocument`] is built from one pulldown-cmark pass and records byte
//! ranges into the parsed text. Handlers convert those offsets to protocol
//! positions through the matching [`lspf::Document`] snapshot, so the
//! negotiated position encoding stays the framework's concern.

mod defs;

use std::ops::Range;

use pulldown_cmark::{BrokenLink, CowStr, Event, LinkType, Options, Parser, Tag, TagEnd};

use crate::slug::SlugBuilder;

pub(crate) use defs::normalize_label;

/// One ATX or setext heading.
#[derive(Debug, Clone)]
pub(crate) struct Heading {
    pub(crate) level: u8,
    /// The heading's inline source, markup included.
    pub(crate) title: String,
    /// The document-unique GitHub slug of the rendered text.
    pub(crate) slug: String,
    /// The inline content, without markers.
    pub(crate) content: Range<usize>,
    /// The whole heading, markers and setext underline included.
    pub(crate) range: Range<usize>,
    /// Where the section this heading opens ends.
    pub(crate) section_end: usize,
}

/// An inline link, image, or autolink whose destination is written in place.
#[derive(Debug, Clone)]
pub(crate) struct MdLink {
    /// The destination as written, without angle brackets.
    pub(crate) href: String,
    pub(crate) href_range: Range<usize>,
    pub(crate) range: Range<usize>,
    pub(crate) image: bool,
    pub(crate) autolink: bool,
}

/// A use of a link reference definition: `[text][label]`, `[label][]`, or
/// `[label]`.
#[derive(Debug, Clone)]
pub(crate) struct MdReference {
    pub(crate) label: String,
    pub(crate) label_range: Range<usize>,
    pub(crate) image: bool,
}

/// A link reference definition, `[label]: destination`.
#[derive(Debug, Clone)]
pub(crate) struct LinkDef {
    pub(crate) label: String,
    pub(crate) label_range: Range<usize>,
    pub(crate) dest: String,
    pub(crate) dest_range: Range<usize>,
    /// The whole definition line, without its line ending.
    pub(crate) range: Range<usize>,
}

/// A full or collapsed reference whose label has no definition.
#[derive(Debug, Clone)]
pub(crate) struct BrokenReference {
    pub(crate) label: String,
    pub(crate) label_range: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockKind {
    Paragraph,
    Heading,
    BlockQuote,
    CodeBlock,
    HtmlBlock,
    List,
    Item,
    Table,
    TableRow,
    Metadata,
    Footnote,
    Emphasis,
    Strong,
    Strikethrough,
    InlineCode,
    Link,
}

impl BlockKind {
    pub(crate) fn is_inline(self) -> bool {
        matches!(
            self,
            Self::Emphasis | Self::Strong | Self::Strikethrough | Self::InlineCode | Self::Link
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Block {
    pub(crate) kind: BlockKind,
    pub(crate) range: Range<usize>,
}

/// The parsed shape of one Markdown text.
#[derive(Debug, Clone, Default)]
pub(crate) struct MdDocument {
    pub(crate) headings: Vec<Heading>,
    pub(crate) links: Vec<MdLink>,
    pub(crate) references: Vec<MdReference>,
    pub(crate) definitions: Vec<LinkDef>,
    pub(crate) broken_references: Vec<BrokenReference>,
    /// Block and inline spans in source order, outer before inner.
    pub(crate) blocks: Vec<Block>,
    /// `<!-- #region -->` … `<!-- #endregion -->` pairs.
    pub(crate) regions: Vec<Range<usize>>,
}

impl MdDocument {
    pub(crate) fn heading_for_fragment(&self, fragment: &str) -> Option<&Heading> {
        self.headings
            .iter()
            .find(|heading| crate::slug::fragment_matches(fragment, &heading.slug))
    }

    pub(crate) fn definition(&self, label: &str) -> Option<&LinkDef> {
        let label = normalize_label(label);
        self.definitions
            .iter()
            .find(|definition| normalize_label(&definition.label) == label)
    }
}

struct Frame {
    kind: Option<BlockKind>,
    link: Option<(LinkType, bool)>,
    range: Range<usize>,
    inner_end: Option<usize>,
    heading: Option<HeadingText>,
}

#[derive(Default)]
struct HeadingText {
    level: u8,
    rendered: String,
    content: Option<Range<usize>>,
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}

fn block_kind(tag: &Tag<'_>) -> Option<BlockKind> {
    Some(match tag {
        Tag::Paragraph => BlockKind::Paragraph,
        Tag::Heading { .. } => BlockKind::Heading,
        Tag::BlockQuote(_) => BlockKind::BlockQuote,
        Tag::CodeBlock(_) => BlockKind::CodeBlock,
        Tag::HtmlBlock => BlockKind::HtmlBlock,
        Tag::List(_) => BlockKind::List,
        Tag::Item => BlockKind::Item,
        Tag::Table(_) => BlockKind::Table,
        Tag::TableHead | Tag::TableRow => BlockKind::TableRow,
        Tag::MetadataBlock(_) => BlockKind::Metadata,
        Tag::FootnoteDefinition(_) => BlockKind::Footnote,
        Tag::Emphasis => BlockKind::Emphasis,
        Tag::Strong => BlockKind::Strong,
        Tag::Strikethrough => BlockKind::Strikethrough,
        Tag::Link { .. } | Tag::Image { .. } => BlockKind::Link,
        _ => return None,
    })
}

fn trim_end(text: &str, range: Range<usize>) -> Range<usize> {
    let trimmed = text[range.clone()].trim_end();
    range.start..range.start + trimmed.len()
}

/// Parse `text` into the document model.
pub(crate) fn parse(text: &str) -> MdDocument {
    let mut broken = Vec::new();
    let callback = |link: BrokenLink<'_>| -> Option<(CowStr<'_>, CowStr<'_>)> {
        if matches!(link.link_type, LinkType::Reference | LinkType::Collapsed) {
            broken.push((link.span, link.link_type));
        }
        None
    };
    let parser = Parser::new_with_broken_link_callback(text, options(), Some(callback));
    let mut events = parser.into_offset_iter();
    let mut document = MdDocument::default();
    let mut stack: Vec<Frame> = Vec::new();
    let mut slugs = SlugBuilder::default();
    let mut leaf_ranges: Vec<Range<usize>> = Vec::new();

    for (event, range) in events.by_ref() {
        if let Some(parent) = stack.last_mut()
            && !matches!(event, Event::End(_))
        {
            parent.inner_end = Some(parent.inner_end.map_or(range.end, |end| end.max(range.end)));
        }
        if let Some(heading) = stack
            .iter_mut()
            .rev()
            .find_map(|frame| frame.heading.as_mut())
            && !matches!(event, Event::End(_) | Event::Start(Tag::Heading { .. }))
        {
            heading.content = Some(match heading.content.take() {
                Some(content) => content.start.min(range.start)..content.end.max(range.end),
                None => range.clone(),
            });
            if let Event::Text(value) | Event::Code(value) = &event {
                heading.rendered.push_str(value);
            }
        }
        match event {
            Event::Start(tag) => {
                let link = match &tag {
                    Tag::Link { link_type, .. } => Some((*link_type, false)),
                    Tag::Image { link_type, .. } => Some((*link_type, true)),
                    _ => None,
                };
                let heading = match &tag {
                    Tag::Heading { level, .. } => Some(HeadingText {
                        level: *level as u8,
                        ..HeadingText::default()
                    }),
                    _ => None,
                };
                stack.push(Frame {
                    kind: block_kind(&tag),
                    link,
                    range,
                    inner_end: None,
                    heading,
                });
            }
            Event::End(tag_end) => {
                let Some(frame) = stack.pop() else { continue };
                if let Some(parent) = stack.last_mut() {
                    parent.inner_end = Some(
                        parent
                            .inner_end
                            .map_or(frame.range.end, |end| end.max(frame.range.end)),
                    );
                }
                finish_frame(text, &mut document, &mut slugs, frame);
                if matches!(
                    tag_end,
                    TagEnd::Paragraph
                        | TagEnd::Heading(_)
                        | TagEnd::CodeBlock
                        | TagEnd::HtmlBlock
                        | TagEnd::Table
                        | TagEnd::MetadataBlock(_)
                ) {
                    leaf_ranges.push(range);
                }
            }
            Event::Code(_) => document.blocks.push(Block {
                kind: BlockKind::InlineCode,
                range,
            }),
            _ => {}
        }
    }

    let refdefs = events.reference_definitions();
    document.definitions =
        defs::definitions(text, &leaf_ranges, |label| refdefs.get(label).is_some());
    document.broken_references = broken
        .into_iter()
        .filter_map(|(span, link_type)| broken_reference(text, span, link_type))
        .collect();
    document.blocks.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then(b.range.end.cmp(&a.range.end))
    });
    document.regions = regions(text, &document.blocks);
    close_sections(&mut document.headings, text.len());
    document
}

fn finish_frame(text: &str, document: &mut MdDocument, slugs: &mut SlugBuilder, frame: Frame) {
    if let Some(heading) = &frame.heading
        && let Some(content) = heading.content.clone()
    {
        let content = trim_end(text, content);
        let title = text[content.clone()].to_string();
        if !title.is_empty() {
            document.headings.push(Heading {
                level: heading.level,
                slug: slugs.add(&heading.rendered),
                title,
                content,
                range: trim_end(text, frame.range.clone()),
                section_end: text.len(),
            });
        }
    }
    if let Some((link_type, image)) = frame.link {
        record_link(text, document, &frame, link_type, image);
    }
    if let Some(kind) = frame.kind {
        let range = if kind.is_inline() {
            frame.range
        } else {
            trim_end(text, frame.range)
        };
        document.blocks.push(Block { kind, range });
    }
}

fn record_link(
    text: &str,
    document: &mut MdDocument,
    frame: &Frame,
    link_type: LinkType,
    image: bool,
) {
    let range = frame.range.clone();
    let open = range.start + if image { 2 } else { 1 };
    match link_type {
        LinkType::Inline => {
            let from = frame.inner_end.unwrap_or(open).max(open);
            let Some(close) = text[from..range.end].find("](") else {
                return;
            };
            let destination_start = from + close + 2;
            let Some(href_range) = inline_destination(text, destination_start, range.end) else {
                return;
            };
            document.links.push(MdLink {
                href: text[href_range.clone()].to_string(),
                href_range,
                range,
                image,
                autolink: false,
            });
        }
        LinkType::Autolink => {
            if range.end - range.start < 2 {
                return;
            }
            let href_range = range.start + 1..range.end - 1;
            document.links.push(MdLink {
                href: text[href_range.clone()].to_string(),
                href_range,
                range,
                image,
                autolink: true,
            });
        }
        LinkType::Reference | LinkType::Collapsed | LinkType::Shortcut => {
            let Some(label_range) = reference_label(text, range.clone(), link_type, open) else {
                return;
            };
            document.references.push(MdReference {
                label: text[label_range.clone()].to_string(),
                label_range,
                image,
            });
        }
        _ => {}
    }
}

fn reference_label(
    text: &str,
    range: Range<usize>,
    link_type: LinkType,
    open: usize,
) -> Option<Range<usize>> {
    let source = &text[range.clone()];
    match link_type {
        LinkType::Reference => {
            let label_open = source.rfind('[')?;
            (source.ends_with(']') && label_open + 1 < source.len())
                .then(|| range.start + label_open + 1..range.end - 1)
        }
        // pulldown-cmark ends a collapsed reference's span before its `[]`.
        LinkType::Collapsed if source.ends_with("][]") && open <= range.end - 3 => {
            Some(open..range.end - 3)
        }
        LinkType::Collapsed | LinkType::Shortcut => {
            (source.ends_with(']') && open < range.end).then(|| open..range.end - 1)
        }
        _ => None,
    }
}

fn broken_reference(
    text: &str,
    span: Range<usize>,
    link_type: LinkType,
) -> Option<BrokenReference> {
    let open = span.start
        + if text[span.clone()].starts_with('!') {
            2
        } else {
            1
        };
    let label_range = reference_label(text, span, link_type, open)?;
    Some(BrokenReference {
        label: text[label_range.clone()].to_string(),
        label_range,
    })
}

/// Find an inline link destination starting at `start`, returning its range
/// without angle brackets.
pub(crate) fn inline_destination(text: &str, start: usize, limit: usize) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let mut cursor = start;
    while cursor < limit && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if bytes.get(cursor) == Some(&b'<') {
        let end = cursor + 1 + text[cursor + 1..limit].find('>')?;
        return Some(cursor + 1..end);
    }
    let destination_start = cursor;
    let mut depth = 0usize;
    while cursor < limit {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(limit),
            b'(' => {
                depth += 1;
                cursor += 1;
            }
            b')' if depth > 0 => {
                depth -= 1;
                cursor += 1;
            }
            b')' => break,
            byte if byte.is_ascii_whitespace() => break,
            _ => cursor += 1,
        }
    }
    Some(destination_start..cursor)
}

fn close_sections(headings: &mut [Heading], len: usize) {
    for index in 0..headings.len() {
        let level = headings[index].level;
        headings[index].section_end = headings[index + 1..]
            .iter()
            .find(|next| next.level <= level)
            .map_or(len, |next| next.range.start);
    }
}

fn region_marker(source: &str) -> Option<bool> {
    let comment = source.trim_start().strip_prefix("<!--")?.trim_start();
    let comment = comment.strip_prefix('#').unwrap_or(comment);
    let is_word_end = |rest: &str| {
        rest.chars()
            .next()
            .is_none_or(|next| !next.is_alphanumeric() && next != '_')
    };
    if let Some(rest) = comment.strip_prefix("endregion") {
        return is_word_end(rest).then_some(false);
    }
    let rest = comment.strip_prefix("region")?;
    is_word_end(rest).then_some(true)
}

fn regions(text: &str, blocks: &[Block]) -> Vec<Range<usize>> {
    let mut open = Vec::new();
    let mut regions = Vec::new();
    for block in blocks
        .iter()
        .filter(|block| block.kind == BlockKind::HtmlBlock)
    {
        match region_marker(&text[block.range.clone()]) {
            Some(true) => open.push(block.range.start),
            Some(false) => {
                if let Some(start) = open.pop() {
                    regions.push(start..block.range.end);
                }
            }
            None => {}
        }
    }
    regions.sort_by_key(|region| region.start);
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(text: &str, range: &Range<usize>) -> String {
        text[range.clone()].to_string()
    }

    #[test]
    fn headings_record_levels_slugs_and_sections() {
        let text =
            "# Title\n\nintro\n\n## Part *one*\n\nbody\n\n## Part one\n\nSetext\n---\n\n# Next\n";
        let document = parse(text);
        let titles: Vec<_> = document
            .headings
            .iter()
            .map(|heading| (heading.level, heading.title.as_str(), heading.slug.as_str()))
            .collect();
        assert_eq!(
            titles,
            [
                (1, "Title", "title"),
                (2, "Part *one*", "part-one"),
                (2, "Part one", "part-one-1"),
                (2, "Setext", "setext"),
                (1, "Next", "next"),
            ]
        );
        let title = &document.headings[0];
        assert_eq!(title.section_end, text.find("# Next").unwrap());
        assert_eq!(slice(text, &title.range), "# Title");
        let setext = &document.headings[3];
        assert_eq!(slice(text, &setext.range), "Setext\n---");
        assert_eq!(slice(text, &setext.content), "Setext");
    }

    #[test]
    fn inline_links_record_destination_ranges() {
        let text = "See [a](one.md) and ![img](<two words.png> \"t\") and [p](guide_(v2).md#x).\n";
        let document = parse(text);
        let hrefs: Vec<_> = document
            .links
            .iter()
            .map(|link| (slice(text, &link.href_range), link.image))
            .collect();
        assert_eq!(
            hrefs,
            [
                ("one.md".to_string(), false),
                ("two words.png".to_string(), true),
                ("guide_(v2).md#x".to_string(), false),
            ]
        );
        assert_eq!(slice(text, &document.links[0].range), "[a](one.md)");
    }

    #[test]
    fn nested_link_text_does_not_hide_the_destination() {
        let text = "[a [b] *c*](target.md)\n";
        let document = parse(text);
        assert_eq!(document.links[0].href, "target.md");
    }

    #[test]
    fn autolinks_are_links_without_brackets() {
        let text = "Visit <https://example.com>.\n";
        let document = parse(text);
        assert_eq!(document.links[0].href, "https://example.com");
        assert!(document.links[0].autolink);
    }

    #[test]
    fn references_and_definitions_are_linked_by_label() {
        let text =
            "[full][Docs] [Docs][] [docs]\n\n[docs]: guide.md \"Title\"\n  [other]: <a b.md>\n";
        let document = parse(text);
        let labels: Vec<_> = document
            .references
            .iter()
            .map(|reference| slice(text, &reference.label_range))
            .collect();
        assert_eq!(labels, ["Docs", "Docs", "docs"]);
        let defs: Vec<_> = document
            .definitions
            .iter()
            .map(|definition| (definition.label.as_str(), definition.dest.as_str()))
            .collect();
        assert_eq!(defs, [("docs", "guide.md"), ("other", "a b.md")]);
        assert_eq!(slice(text, &document.definitions[1].dest_range), "a b.md");
        assert_eq!(document.definition("DOCS").unwrap().dest, "guide.md");
    }

    #[test]
    fn duplicate_definitions_are_kept_in_order() {
        let text = "[a]\n\n[a]: first.md\n[a]: second.md\n";
        let document = parse(text);
        let dests: Vec<_> = document
            .definitions
            .iter()
            .map(|definition| definition.dest.as_str())
            .collect();
        assert_eq!(dests, ["first.md", "second.md"]);
    }

    #[test]
    fn definition_syntax_in_code_or_paragraphs_is_not_a_definition() {
        let text = "```\n[a]: code.md\n```\n\n    [b]: indented.md\n\ntext\n[c]: lazy.md\n\n> [d]: quoted.md\n";
        let document = parse(text);
        let labels: Vec<_> = document
            .definitions
            .iter()
            .map(|definition| definition.label.as_str())
            .collect();
        assert_eq!(labels, ["d"]);
    }

    #[test]
    fn broken_full_and_collapsed_references_are_reported() {
        let text = "[text][missing] [gone][] [shortcut]\n";
        let document = parse(text);
        let labels: Vec<_> = document
            .broken_references
            .iter()
            .map(|reference| reference.label.as_str())
            .collect();
        assert_eq!(labels, ["missing", "gone"]);
    }

    #[test]
    fn code_and_escapes_hide_link_syntax() {
        let text = "`[a](x.md)` \\[b](y.md)\n\n```\n[c](z.md)\n```\n";
        let document = parse(text);
        assert!(document.links.is_empty());
        assert!(
            document
                .blocks
                .iter()
                .any(|block| block.kind == BlockKind::InlineCode)
        );
    }

    #[test]
    fn regions_pair_nested_markers() {
        let text = "<!-- #region outer -->\n\n<!-- region inner -->\n\ntext\n\n<!-- endregion -->\n\n<!-- #endregion -->\n\n<!-- #regional -->\n";
        let document = parse(text);
        let regions: Vec<_> = document
            .regions
            .iter()
            .map(|region| text[..region.start].lines().count())
            .collect();
        assert_eq!(regions, [0, 2]);
        assert_eq!(document.regions.len(), 2);
    }

    #[test]
    fn front_matter_is_not_a_heading() {
        let text = "---\ntitle: x\n---\n\n# Real\n";
        let document = parse(text);
        assert_eq!(document.headings.len(), 1);
        assert_eq!(document.headings[0].title, "Real");
    }
}
