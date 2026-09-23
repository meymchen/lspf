//! Link destination resolution and URI path arithmetic.

use std::str::FromStr;

use lspf::types::Uri;

/// A link destination resolved to a local resource.
#[derive(Debug, Clone)]
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

/// Whether `href` names a resource with a non-`file` scheme, such as
/// `https:` or `mailto:`. Those links are left to their owning clients.
pub(crate) fn is_external(href: &str) -> bool {
    href.starts_with("//") || has_uri_scheme(href) && !href.starts_with("file:")
}

/// Split a URI string into the prefix up to its path and the path itself.
fn split_path(uri: &str) -> (&str, &str) {
    let Some(colon) = uri.find(':') else {
        return ("", uri);
    };
    let after_scheme = colon + 1;
    let path_start = if uri[after_scheme..].starts_with("//") {
        uri[after_scheme + 2..]
            .find('/')
            .map_or(uri.len(), |slash| after_scheme + 2 + slash)
    } else {
        after_scheme
    };
    uri.split_at(path_start)
}

fn normalize_uri_path(uri: &str) -> String {
    let (prefix, path) = split_path(uri);
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

/// Decode `%XX` escapes, keeping malformed escapes and invalid UTF-8 as
/// written.
pub(crate) fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = value.get(index + 1..index + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).unwrap_or_else(|_| value.to_string())
}

/// Percent-encode the bytes a URI path cannot carry literally.
pub(crate) fn encode_path(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'/'
                    | b'%'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
            )
        {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The URI without its query or fragment.
pub(crate) fn without_fragment(uri: &str) -> &str {
    uri.split(['#', '?']).next().unwrap_or_default()
}

/// A comparison key that treats equivalent spellings of one resource alike:
/// percent-encoding, scheme case, and Windows drive-letter case.
pub(crate) fn uri_key(uri: &Uri) -> String {
    let decoded = percent_decode(without_fragment(uri.as_str()));
    let (prefix, path) = split_path(&decoded);
    let mut path = path.to_string();
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        path.replace_range(1..2, &path[1..2].to_ascii_lowercase());
    }
    format!(
        "{}{}",
        prefix.to_ascii_lowercase(),
        path.trim_end_matches('/')
    )
}

/// Whether `uri` is `root` or lies beneath it.
pub(crate) fn is_within(uri: &Uri, root: &Uri) -> bool {
    let uri = uri_key(uri);
    let root = uri_key(root);
    uri == root || uri.starts_with(&format!("{root}/"))
}

/// Append a child name to a directory URI.
pub(crate) fn child(directory: &Uri, name: &str) -> Option<Uri> {
    let base = without_fragment(directory.as_str()).trim_end_matches('/');
    Uri::from_str(&format!("{base}/{}", encode_path(name))).ok()
}

/// Resolve `target`, as written in the document at `source`, to a local
/// resource. Root-relative targets resolve against `root` when the document
/// lives in a workspace root, and against the URI's own root otherwise.
pub(crate) fn resolve_local_target(
    source: &Uri,
    target: &str,
    root: Option<&Uri>,
) -> Option<LocalTarget> {
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

    let target = encode_path(target);
    let source = without_fragment(source.as_str());
    let combined = if target.starts_with('/') {
        if let Some(root) = root {
            format!(
                "{}{target}",
                without_fragment(root.as_str()).trim_end_matches('/')
            )
        } else {
            let (prefix, _) = split_path(source);
            format!("{prefix}{target}")
        }
    } else {
        let directory_end = source.rfind('/')? + 1;
        format!("{}{target}", &source[..directory_end])
    };
    Uri::from_str(&normalize_uri_path(&combined))
        .ok()
        .map(|uri| LocalTarget { uri, fragment })
}

/// The relative path from the directory of `from` to `to`, when both share a
/// scheme and authority.
pub(crate) fn relative_path(from: &Uri, to: &Uri) -> Option<String> {
    let (from_prefix, from_path) = split_path(without_fragment(from.as_str()));
    let (to_prefix, to_path) = split_path(without_fragment(to.as_str()));
    if !from_prefix.eq_ignore_ascii_case(to_prefix) {
        return None;
    }
    let from_segments: Vec<_> = from_path.split('/').filter(|s| !s.is_empty()).collect();
    let to_segments: Vec<_> = to_path.split('/').filter(|s| !s.is_empty()).collect();
    let from_directory = &from_segments[..from_segments.len().saturating_sub(1)];
    let common = from_directory
        .iter()
        .zip(&to_segments)
        .take_while(|(a, b)| percent_decode(a).eq_ignore_ascii_case(&percent_decode(b)))
        .count();
    let mut parts: Vec<&str> = vec![".."; from_directory.len() - common];
    parts.extend(&to_segments[common..]);
    Some(parts.join("/"))
}

/// The path of `to` below `root`, with a leading slash.
pub(crate) fn root_relative_path(root: &Uri, to: &Uri) -> Option<String> {
    let root = without_fragment(root.as_str()).trim_end_matches('/');
    let to = without_fragment(to.as_str());
    to.get(..root.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(root))
        .map(|_| to[root.len()..].to_string())
        .filter(|path| path.starts_with('/'))
}

/// The final path segment of `uri`, percent-decoded.
pub(crate) fn file_name(uri: &Uri) -> String {
    let path = without_fragment(uri.as_str()).trim_end_matches('/');
    percent_decode(&path[path.rfind('/').map_or(0, |slash| slash + 1)..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(value: &str) -> Uri {
        Uri::from_str(value).unwrap()
    }

    #[test]
    fn relative_targets_resolve_against_the_source_directory() {
        let source = uri("file:///w/docs/readme.md");
        let target = resolve_local_target(&source, "../guide.md#Install", None).unwrap();
        assert_eq!(target.uri.as_str(), "file:///w/guide.md");
        assert_eq!(target.fragment.as_deref(), Some("Install"));
        let spaced = resolve_local_target(&source, "my file.md", None).unwrap();
        assert_eq!(spaced.uri.as_str(), "file:///w/docs/my%20file.md");
        let own = resolve_local_target(&source, "#top", None).unwrap();
        assert_eq!(own.uri, source);
    }

    #[test]
    fn root_relative_targets_prefer_the_workspace_root() {
        let source = uri("file:///w/docs/readme.md");
        let root = uri("file:///w/");
        let target = resolve_local_target(&source, "/guide.md", Some(&root)).unwrap();
        assert_eq!(target.uri.as_str(), "file:///w/guide.md");
        let fallback = resolve_local_target(&source, "/guide.md", None).unwrap();
        assert_eq!(fallback.uri.as_str(), "file:///guide.md");
    }

    #[test]
    fn external_targets_are_not_local() {
        let source = uri("file:///w/readme.md");
        assert!(resolve_local_target(&source, "https://example.com", None).is_none());
        assert!(resolve_local_target(&source, "//cdn/x.png", None).is_none());
        assert!(is_external("mailto:a@b.c"));
        assert!(!is_external("file:///w/a.md"));
        assert!(!is_external("guide.md"));
    }

    #[test]
    fn keys_ignore_encoding_and_drive_case() {
        assert_eq!(
            uri_key(&uri("file:///C%3A/Docs/a%20b.md")),
            uri_key(&uri("file:///c:/Docs/a b.md".replace(' ', "%20").as_str()))
        );
        assert!(is_within(
            &uri("file:///w/docs/a.md"),
            &uri("file:///w/docs")
        ));
        assert!(!is_within(
            &uri("file:///w/docs2/a.md"),
            &uri("file:///w/docs")
        ));
    }

    #[test]
    fn relative_paths_walk_up_and_down() {
        let from = uri("file:///w/docs/readme.md");
        assert_eq!(
            relative_path(&from, &uri("file:///w/docs/guide.md")).as_deref(),
            Some("guide.md")
        );
        assert_eq!(
            relative_path(&from, &uri("file:///w/other/x.md")).as_deref(),
            Some("../other/x.md")
        );
        assert_eq!(
            root_relative_path(&uri("file:///w"), &uri("file:///w/a/b.md")).as_deref(),
            Some("/a/b.md")
        );
        assert_eq!(file_name(&uri("file:///w/a%20b.md")), "a b.md");
        assert_eq!(
            child(&uri("file:///w/"), "a b.md").unwrap().as_str(),
            "file:///w/a%20b.md"
        );
    }
}
