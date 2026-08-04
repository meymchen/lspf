//! The one normalized URI identity key (ADR 0021).
//!
//! Two client-supplied URIs name the same resource — and so share one
//! [`UriKey`] — when they differ only in scheme or authority case, in
//! percent-encoding, or in the case of a Windows drive letter in a `file:`
//! path. Ordinary path case is preserved: the key must not merge
//! `file:///Foo` with `file:///foo` on a case-sensitive filesystem.
//!
//! The key exists for framework-internal identity only (the `Documents`
//! map). Public values and wire responses always carry the client's original
//! URI, never the normalized form.

use lsp_types::Uri;

/// The normalized identity of a client-supplied [`Uri`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct UriKey {
    /// The scheme, lowercased (`FILE` and `file` are one scheme).
    scheme: String,
    /// The authority with only its host lowercased: host names are
    /// case-insensitive, but userinfo is case-sensitive per RFC 3986.
    authority: String,
    /// The path, percent-decoded; a leading Windows drive letter is
    /// lowercased for `file:` URIs, everything else keeps its case.
    path: String,
    /// The query, percent-decoded like the path.
    query: String,
}

impl UriKey {
    pub(crate) fn new(uri: &Uri) -> Self {
        let scheme = uri
            .scheme()
            .map(|scheme| scheme.as_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let authority = uri
            .authority()
            .map(|authority| {
                let mut normalized = String::new();
                if let Some(userinfo) = authority.userinfo() {
                    normalized.push_str(userinfo.as_str());
                    normalized.push('@');
                }
                normalized.push_str(&authority.host().as_str().to_ascii_lowercase());
                if let Some(port) = authority.port() {
                    normalized.push(':');
                    normalized.push_str(port);
                }
                normalized
            })
            .unwrap_or_default();
        let path = percent_decode(uri.path().as_str());
        let path = if scheme == "file" {
            lowercase_drive_letter(path)
        } else {
            path
        };
        let query = uri
            .query()
            .map(|query| percent_decode(query.as_str()))
            .unwrap_or_default();
        Self {
            scheme,
            authority,
            path,
            query,
        }
    }
}

/// Decode every `%XX` triplet to its byte, leaving a malformed triplet
/// untouched. Decoding — not just hex-case folding — is what makes `c%3A`
/// and `c:` the same key. Lossy UTF-8 keeps even a pathologically encoded
/// path deterministic.
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 3 <= bytes.len()
            && let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            decoded.push(hi * 16 + lo);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Lowercase a leading Windows drive letter — `/C:` → `/c:` — leaving every
/// later path byte, including its case, untouched. Windows filesystems are
/// case-insensitive, so `C:` and `c:` name one drive; the rest of the path
/// is not normalized away because the key cannot know the filesystem's
/// case sensitivity beyond the drive letter itself.
fn lowercase_drive_letter(path: String) -> String {
    let drive_letter = {
        let bytes = path.as_bytes();
        let is_drive = bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
            && (bytes.len() == 3 || bytes[3] == b'/');
        is_drive.then(|| (bytes[1] as char).to_ascii_lowercase())
    };
    let Some(lower) = drive_letter else {
        return path;
    };
    let mut path = path;
    path.replace_range(1..2, &lower.to_string());
    path
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn key(spelling: &str) -> UriKey {
        UriKey::new(&Uri::from_str(spelling).expect("the test URI parses"))
    }

    #[test]
    fn scheme_and_host_case_are_normalized() {
        assert_eq!(key("FILE:///a.rs"), key("file:///a.rs"));
        assert_eq!(
            key("file://Example.COM/a.rs"),
            key("file://example.com/a.rs")
        );
    }

    #[test]
    fn ordinary_path_case_is_preserved() {
        assert_ne!(
            key("file:///Foo/Bar.rs"),
            key("file:///foo/Bar.rs"),
            "case-sensitive filesystems distinguish these, so the key must too"
        );
    }

    #[test]
    fn percent_encoded_equivalents_share_one_key() {
        assert_eq!(key("file:///%41.rs"), key("file:///A.rs"));
        assert_eq!(key("file:///a%2fb"), key("file:///a%2Fb"));
        assert_eq!(key("file:///a%2Fb"), key("file:///a/b"));
        assert_eq!(
            key("untitled:///a.rs?ref=%48EAD"),
            key("untitled:///a.rs?ref=HEAD"),
            "the query decodes by the same rule"
        );
    }

    #[test]
    fn windows_drive_letter_case_is_normalized_for_file_uris() {
        assert_eq!(key("file:///C:/Users/Me"), key("file:///c:/Users/Me"));
        assert_eq!(
            key("file:///C%3A/Users/Me"),
            key("file:///c:/Users/Me"),
            "an encoded drive colon is the same drive"
        );
        assert_eq!(
            key("file:///C%3a/Users/Me"),
            key("file:///c:/Users/Me"),
            "hex-digit case is irrelevant"
        );
        assert_ne!(
            key("file:///c:/USERS/Me"),
            key("file:///c:/Users/Me"),
            "only the drive letter itself loses its case"
        );
    }

    #[test]
    fn drive_letter_normalization_is_file_scheme_only() {
        assert_ne!(
            key("untitled:/C:/a.rs"),
            key("untitled:/c:/a.rs"),
            "a drive letter is `file:` semantics, not path semantics"
        );
    }

    #[test]
    fn distinct_documents_stay_distinct() {
        assert_ne!(key("file:///a.rs"), key("file:///b.rs"));
        assert_ne!(key("file:///a.rs"), key("untitled:///a.rs"));
    }

    #[test]
    fn userinfo_case_is_preserved_while_host_case_is_normalized() {
        assert_eq!(
            key("foo://Alice@Example.COM/x"),
            key("foo://Alice@example.com/x"),
            "the host is case-insensitive"
        );
        assert_ne!(
            key("foo://Alice@h/x"),
            key("foo://alice@h/x"),
            "the userinfo is case-sensitive per RFC 3986"
        );
    }
}
