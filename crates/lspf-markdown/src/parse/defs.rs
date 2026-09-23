//! Link reference definition scanning.
//!
//! pulldown-cmark resolves definitions but keeps only the first definition of
//! each label and reports no label or destination ranges. This scanner walks
//! the source lines outside code blocks, strips block-quote and list
//! prefixes, and keeps every single-line `[label]: destination`. A candidate
//! counts only when it lies outside every leaf block the parser produced and
//! the parser also knows its label, so paragraph text and code never qualify.

use std::ops::Range;

use super::LinkDef;

#[derive(Clone, Copy, Debug)]
struct SourceLine<'a> {
    text: &'a str,
    start: usize,
}

fn blockquote_content(line: &str) -> (&str, usize) {
    let mut content = line;
    let mut removed = 0;
    loop {
        let spaces = content.bytes().take_while(|byte| *byte == b' ').count();
        if spaces > 3 || content.as_bytes().get(spaces) != Some(&b'>') {
            return (content, removed);
        }
        let mut prefix = spaces + 1;
        if matches!(content.as_bytes().get(prefix), Some(b' ' | b'\t')) {
            prefix += 1;
        }
        content = &content[prefix..];
        removed += prefix;
    }
}

fn indentation(content: &str) -> (usize, usize) {
    let mut bytes = 0;
    let mut columns = 0;
    for byte in content.bytes() {
        match byte {
            b' ' => columns += 1,
            b'\t' => columns += 4 - columns % 4,
            _ => break,
        }
        bytes += 1;
    }
    (bytes, columns)
}

fn list_marker_width(trimmed: &str) -> Option<usize> {
    if matches!(trimmed.as_bytes(), [b'-' | b'+' | b'*', b' ' | b'\t', ..]) {
        return Some(2);
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0
        && matches!(trimmed.as_bytes().get(digits), Some(b'.' | b')'))
        && matches!(trimmed.as_bytes().get(digits + 1), Some(b' ' | b'\t')))
    .then_some(digits + 2)
}

fn content_lines(text: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let (content, quote_prefix) = blockquote_content(line);
        let (indent_bytes, indent_columns) = indentation(content);
        let mut trimmed = &content[indent_bytes..];
        let mut offset = start + quote_prefix + indent_bytes;
        if indent_columns <= 3
            && let Some(width) = list_marker_width(trimmed)
        {
            let after = trimmed[width..].trim_start_matches([' ', '\t']);
            offset += trimmed.len() - after.len();
            trimmed = after;
        }
        lines.push(SourceLine {
            text: trimmed.trim_end_matches(['\r', '\n']),
            start: offset,
        });
        start += line.len();
    }
    lines
}

/// Normalize a reference label the way CommonMark matches them: collapse
/// internal whitespace and compare case-insensitively.
pub(crate) fn normalize_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn definition(source: SourceLine<'_>) -> Option<LinkDef> {
    let line = source.text;
    let rest = line.strip_prefix('[')?;
    let separator = rest.find("]:")?;
    let label = &rest[..separator];
    if label.trim().is_empty() || label.contains(['[', ']']) {
        return None;
    }
    let after = &rest[separator + 2..];
    let destination = after.trim_start();
    let dest_offset = 1 + separator + 2 + (after.len() - destination.len());
    let (dest, dest_start) = if let Some(inner) = destination.strip_prefix('<') {
        (&inner[..inner.find('>')?], dest_offset + 1)
    } else {
        let end = destination
            .find(char::is_whitespace)
            .unwrap_or(destination.len());
        (&destination[..end], dest_offset)
    };
    if dest.is_empty() {
        return None;
    }
    let dest_range = source.start + dest_start..source.start + dest_start + dest.len();
    Some(LinkDef {
        label: label.to_string(),
        label_range: source.start + 1..source.start + 1 + label.len(),
        dest: dest.to_string(),
        dest_range,
        range: source.start..source.start + line.len(),
    })
}

fn inside(ranges: &[Range<usize>], offset: usize) -> bool {
    ranges
        .iter()
        .any(|range| range.start <= offset && offset < range.end)
}

pub(super) fn definitions(
    text: &str,
    leaf_ranges: &[Range<usize>],
    known: impl Fn(&str) -> bool,
) -> Vec<LinkDef> {
    content_lines(text)
        .into_iter()
        .filter_map(definition)
        .filter(|definition| {
            !inside(leaf_ranges, definition.range.start) && known(&definition.label)
        })
        .collect()
}
