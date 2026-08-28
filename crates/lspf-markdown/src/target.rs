use std::str::FromStr;

use lspf::types::{Range, Uri};
use lspf::{Document, PositionEncoding};

use crate::parser::content_lines;

#[derive(Debug)]
pub(crate) struct LocalTarget {
    pub(crate) uri: Uri,
    pub(crate) fragment: Option<String>,
}

impl LocalTarget {
    pub(crate) fn display(&self) -> String {
        self.fragment.as_ref().map_or_else(
            || self.uri.as_str().to_string(),
            |fragment| format!("{}#{fragment}", self.uri.as_str()),
        )
    }
}

#[derive(Debug)]
pub(crate) struct Heading {
    pub(crate) title: String,
    pub(crate) range: Range,
}

fn has_uri_scheme(target: &str) -> bool {
    let Some(colon) = target.find(':') else {
        return false;
    };
    !target[..colon].is_empty()
        && target[..colon].bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || (index > 0 && (byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')))
        })
}

fn normalize_uri_path(uri: &str) -> String {
    let Some(colon) = uri.find(':') else {
        return uri.to_string();
    };
    let after_scheme = colon + 1;
    let path_start = if uri[after_scheme..].starts_with("//") {
        uri[after_scheme + 2..]
            .find('/')
            .map_or(uri.len(), |slash| after_scheme + 2 + slash)
    } else {
        after_scheme
    };
    let (prefix, path) = uri.split_at(path_start);
    let absolute = path.starts_with('/');
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            value => segments.push(value),
        }
    }
    format!(
        "{prefix}{}{}",
        if absolute { "/" } else { "" },
        segments.join("/")
    )
}

pub(crate) fn resolve_local_target(source: &Uri, target: &str) -> Option<LocalTarget> {
    let (target, fragment) = target
        .split_once('#')
        .map_or((target, None), |(target, fragment)| {
            (target, (!fragment.is_empty()).then(|| fragment.to_string()))
        });
    let target = target.split('?').next().unwrap_or_default();
    if target.is_empty() {
        return Some(LocalTarget {
            uri: source.clone(),
            fragment,
        });
    }
    if target.starts_with("//") {
        return None;
    }
    if has_uri_scheme(target) {
        return target
            .starts_with("file:")
            .then(|| Uri::from_str(target).ok())
            .flatten()
            .map(|uri| LocalTarget { uri, fragment });
    }

    let source = source.as_str().split(['#', '?']).next().unwrap_or_default();
    let combined = if target.starts_with('/') {
        let colon = source.find(':')?;
        let after_scheme = colon + 1;
        if source[after_scheme..].starts_with("//") {
            let authority_end = source[after_scheme + 2..]
                .find('/')
                .map_or(source.len(), |slash| after_scheme + 2 + slash);
            format!("{}{}", &source[..authority_end], target)
        } else {
            format!("{}{}", &source[..after_scheme], target)
        }
    } else {
        let directory_end = source.rfind('/')? + 1;
        format!("{}{target}", &source[..directory_end])
    };
    Uri::from_str(&normalize_uri_path(&combined))
        .ok()
        .map(|uri| LocalTarget { uri, fragment })
}

fn heading_slug(title: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in title.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '_' || character == '-' {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character);
        } else if character.is_whitespace() {
            pending_dash = true;
        }
    }
    slug
}

fn headings(document: &Document, encoding: PositionEncoding) -> Vec<(String, Heading)> {
    let text = document.text();
    let lines = content_lines(&text);
    let mut headings = Vec::new();
    for (index, source) in lines.iter().enumerate() {
        let line = source.text.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim_start();
        let indentation = line.len() - trimmed.len();
        if indentation <= 3 {
            let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
            let follows_hashes = trimmed[hashes..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace);
            if (1..=6).contains(&hashes) && follows_hashes {
                let after_hashes = &trimmed[hashes..];
                let spacing = after_hashes.len() - after_hashes.trim_start().len();
                let title = after_hashes.trim().trim_end_matches('#').trim_end();
                push_heading(
                    &mut headings,
                    document,
                    encoding,
                    title,
                    source.start + indentation + hashes + spacing,
                );
                continue;
            }
        }

        let Some(underline) = lines.get(index + 1) else {
            continue;
        };
        if underline.start != source.start + source.text.len() || indentation > 3 {
            continue;
        }
        let marker = underline.text.trim();
        if marker.is_empty()
            || !marker.bytes().all(|byte| byte == b'=' || byte == b'-')
            || marker.contains('=') && marker.contains('-')
        {
            continue;
        }
        let title = trimmed.trim_end();
        push_heading(
            &mut headings,
            document,
            encoding,
            title,
            source.start + indentation,
        );
    }
    headings
}

fn push_heading(
    headings: &mut Vec<(String, Heading)>,
    document: &Document,
    encoding: PositionEncoding,
    title: &str,
    start_offset: usize,
) {
    if title.is_empty() {
        return;
    }
    let end_offset = start_offset + title.len();
    if let (Some(start), Some(end)) = (
        document.offset_to_position(encoding, start_offset),
        document.offset_to_position(encoding, end_offset),
    ) {
        headings.push((
            heading_slug(title),
            Heading {
                title: title.to_string(),
                range: Range::new(start, end),
            },
        ));
    }
}

pub(crate) fn selected_heading(
    document: &Document,
    encoding: PositionEncoding,
    fragment: Option<&str>,
) -> Option<Heading> {
    let headings = headings(document, encoding);
    match fragment {
        Some(fragment) => {
            let fragment = fragment.to_lowercase();
            headings
                .into_iter()
                .find_map(|(slug, heading)| (slug == fragment).then_some(heading))
        }
        None => headings.into_iter().next().map(|(_, heading)| heading),
    }
}
