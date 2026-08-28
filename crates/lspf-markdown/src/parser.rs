use std::collections::HashMap;

#[derive(Debug)]
pub(crate) struct Link {
    pub(crate) target: String,
    pub(crate) target_start: usize,
    pub(crate) target_end: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SourceLine<'a> {
    pub(crate) text: &'a str,
    pub(crate) start: usize,
}

pub(crate) fn content_lines(text: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut fence = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indentation = line.len() - trimmed.len();
        let marker = (indentation <= 3)
            .then(|| trimmed.as_bytes().first().copied())
            .flatten()
            .filter(|byte| matches!(byte, b'`' | b'~'))
            .and_then(|byte| {
                (trimmed
                    .bytes()
                    .take_while(|candidate| *candidate == byte)
                    .count()
                    >= 3)
                    .then_some(byte)
            });
        if let Some(marker) = marker {
            match fence {
                Some(open) if open == marker => fence = None,
                None => fence = Some(marker),
                Some(_) => {}
            }
            start += line.len();
            continue;
        }
        if fence.is_none() && indentation < 4 {
            lines.push(SourceLine { text: line, start });
        }
        start += line.len();
    }
    lines
}

fn normalize_reference_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn inside_inline_code(line: &str, offset: usize) -> bool {
    let bytes = line.as_bytes();
    let mut cursor = 0;
    let mut delimiter = None;
    while cursor < offset {
        if bytes[cursor] != b'`' {
            cursor += 1;
            continue;
        }
        let run_start = cursor;
        while cursor < offset && bytes[cursor] == b'`' {
            cursor += 1;
        }
        let run = cursor - run_start;
        match delimiter {
            Some(open) if open == run => delimiter = None,
            None => delimiter = Some(run),
            Some(_) => {}
        }
    }
    delimiter.is_some()
}

fn inline_destination(line: &str, start: usize) -> Option<(&str, usize, usize, usize)> {
    if line.as_bytes().get(start) == Some(&b'<') {
        let end = line[start + 1..].find('>')? + start + 1;
        let close = line[end + 1..].find(')')? + end + 1;
        return Some((&line[start + 1..end], start + 1, end, close));
    }

    let mut depth = 0;
    let mut cursor = start;
    let mut target_end = None;
    let bytes = line.as_bytes();
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            b'(' if target_end.is_none() => {
                depth += 1;
                cursor += 1;
            }
            b')' if depth > 0 && target_end.is_none() => {
                depth -= 1;
                cursor += 1;
            }
            b')' if depth == 0 => {
                let end = target_end.unwrap_or(cursor);
                return Some((&line[start..end], start, end, cursor));
            }
            byte if byte.is_ascii_whitespace() && depth == 0 => {
                target_end.get_or_insert(cursor);
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    None
}

fn inline_links(lines: &[SourceLine<'_>]) -> Vec<Link> {
    let mut links = Vec::new();
    for source in lines {
        let line = source.text;
        let mut cursor = 0;
        while let Some(relative_close) = line[cursor..].find("](") {
            let close = cursor + relative_close;
            if inside_inline_code(line, close) {
                cursor = close + 2;
                continue;
            }
            let Some(_open) = line[..close].rfind('[') else {
                cursor = close + 2;
                continue;
            };
            let destination_start = close + 2;
            let Some((target, target_start, target_end, destination_end)) =
                inline_destination(line, destination_start)
            else {
                break;
            };
            if !target.is_empty() {
                links.push(Link {
                    target: target.to_string(),
                    target_start: source.start + target_start,
                    target_end: source.start + target_end,
                });
            }
            cursor = destination_end + 1;
        }
    }
    links
}

fn reference_definitions(lines: &[SourceLine<'_>]) -> HashMap<String, String> {
    let mut definitions = HashMap::new();
    for source in lines {
        let line = source.text;
        let trimmed = line.trim_start();
        if line.len() - trimmed.len() > 3 || !trimmed.starts_with('[') {
            continue;
        }
        let Some(separator) = trimmed.find("]:") else {
            continue;
        };
        let label = normalize_reference_label(&trimmed[1..separator]);
        let destination = trimmed[separator + 2..].trim_start();
        let target = if let Some(destination) = destination.strip_prefix('<') {
            destination.find('>').map(|end| &destination[..end])
        } else {
            let end = destination
                .find(char::is_whitespace)
                .unwrap_or(destination.len());
            Some(&destination[..end])
        };
        if !label.is_empty()
            && let Some(target) = target.filter(|target| !target.is_empty())
        {
            definitions.insert(label, target.to_string());
        }
    }
    definitions
}

fn full_reference_links(
    lines: &[SourceLine<'_>],
    definitions: &HashMap<String, String>,
) -> Vec<Link> {
    let mut links = Vec::new();
    for source in lines {
        let line = source.text;
        let mut cursor = 0;
        while let Some(relative_separator) = line[cursor..].find("][") {
            let separator = cursor + relative_separator;
            if inside_inline_code(line, separator) {
                cursor = separator + 2;
                continue;
            }
            let Some(text_open) = line[..separator].rfind('[') else {
                cursor = separator + 2;
                continue;
            };
            let label_start = separator + 2;
            let Some(relative_close) = line[label_start..].find(']') else {
                break;
            };
            let label_end = label_start + relative_close;
            let (label, range_start, range_end) = if label_start == label_end {
                (&line[text_open + 1..separator], text_open + 1, separator)
            } else {
                (&line[label_start..label_end], label_start, label_end)
            };
            if let Some(target) = definitions.get(&normalize_reference_label(label)) {
                links.push(Link {
                    target: target.clone(),
                    target_start: source.start + range_start,
                    target_end: source.start + range_end,
                });
            }
            cursor = label_end + 1;
        }
    }
    links
}

fn shortcut_reference_links(
    lines: &[SourceLine<'_>],
    definitions: &HashMap<String, String>,
) -> Vec<Link> {
    let mut links = Vec::new();
    for source in lines {
        let line = source.text;
        let mut cursor = 0;
        while let Some(relative_open) = line[cursor..].find('[') {
            let open = cursor + relative_open;
            if inside_inline_code(line, open) || open > 0 && line.as_bytes()[open - 1] == b']' {
                cursor = open + 1;
                continue;
            }
            let label_start = open + 1;
            let Some(relative_close) = line[label_start..].find(']') else {
                break;
            };
            let close = label_start + relative_close;
            let next = line.as_bytes().get(close + 1).copied();
            if matches!(next, Some(b'(' | b'[' | b':')) {
                cursor = close + 1;
                continue;
            }
            let label = &line[label_start..close];
            if let Some(target) = definitions.get(&normalize_reference_label(label)) {
                links.push(Link {
                    target: target.clone(),
                    target_start: source.start + label_start,
                    target_end: source.start + close,
                });
            }
            cursor = close + 1;
        }
    }
    links
}

pub(crate) fn markdown_links(text: &str) -> Vec<Link> {
    let lines = content_lines(text);
    let definitions = reference_definitions(&lines);
    let mut links = inline_links(&lines);
    links.extend(full_reference_links(&lines, &definitions));
    links.extend(shortcut_reference_links(&lines, &definitions));
    links
}
