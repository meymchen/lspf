//! Shared helpers for framework unit tests.
//!
//! Compiled only under `#[cfg(test)]`; nothing here is public API.

use std::path::Path;
use std::str::FromStr;

use lsp_types::Uri;

/// An absolute `file:` URI for `path`, percent-encoding every byte that is
/// not legal in a URI path.
pub(crate) fn file_uri(path: &Path) -> Uri {
    let absolute = std::path::absolute(path).expect("the test path is absolute");
    let text = absolute.to_str().expect("test paths are valid UTF-8");
    #[cfg(windows)]
    let text = text.replace('\\', "/");
    // On Windows the drive letter is part of the path, so the URI needs the
    // empty authority spelled out as a third slash: `file:///C:/…`.
    #[cfg(windows)]
    let spelling = format!("file:///{text}");
    #[cfg(not(windows))]
    let spelling = format!("file://{text}");
    Uri::from_str(&percent_encode(&spelling)).expect("the encoded test URI parses")
}

fn percent_encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
