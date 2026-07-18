//! Doc-sync test for the editor-setup documentation (issue #22).
//!
//! The README ships a copy-paste VS Code configuration snippet that launches
//! the `lspf-hello` binary. A typo in the JSON or a drift away from the real
//! binary name silently breaks the MVP onboarding path. Zed cannot register
//! arbitrary servers from settings alone, so its section is pinned to explain
//! the required extension path instead.

use serde_json::Value;

const README: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"));

/// The binary name `cargo install --path crates/lspf-hello` produces. Kept in
/// sync with the `[[bin]]` target in this crate's `Cargo.toml`.
const BINARY: &str = "lspf-hello";

/// Return the body of the section whose heading text equals `heading`,
/// regardless of level (`## VS Code`, `### Zed`, …): everything from the
/// heading line up to (but not including) the next heading at the same or a
/// higher level, or end of file. Level-awareness keeps a `### VS Code`
/// subsection from bleeding into the following `### Zed` subsection.
fn section<'a>(markdown: &'a str, heading: &str) -> &'a str {
    let leading_hashes = |line: &str| line.len() - line.trim_start_matches('#').len();

    let mut offset = 0usize;
    let mut start: Option<usize> = None;
    let mut level = 0usize;
    let mut end = markdown.len();
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let hashes = leading_hashes(trimmed);
        if start.is_none() {
            if hashes > 0 && trimmed[hashes..].trim_start() == heading {
                start = Some(offset);
                level = hashes;
            }
        } else if hashes > 0 && hashes <= level {
            end = offset;
            break;
        }
        offset += line.len();
    }
    let start = start.unwrap_or_else(|| panic!("README is missing a `{heading}` heading"));
    &markdown[start..end]
}

/// Extract every ```json fenced code block found in `markdown`.
fn json_blocks(markdown: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = markdown;
    while let Some(open) = rest.find("```json") {
        let after_open = &rest[open + "```json".len()..];
        let body_start = after_open
            .find('\n')
            .map(|i| i + 1)
            .expect("opening json fence is followed by a newline");
        let body = &after_open[body_start..];
        let close = body.find("```").expect("json fence is closed");
        blocks.push(&body[..close]);
        rest = &body[close + 3..];
    }
    blocks
}

#[test]
fn editor_setup_section_documents_install_command() {
    let setup = section(README, "Editor setup");
    assert!(
        setup.contains("cargo install --path crates/lspf-hello"),
        "Editor setup section must include the install command"
    );
    assert!(
        setup.contains(BINARY),
        "Editor setup section must mention the `{BINARY}` binary"
    );
}

#[test]
fn vscode_snippet_is_valid_json_targeting_the_binary() {
    let vscode = section(README, "VS Code");
    let blocks = json_blocks(vscode);
    assert_eq!(
        blocks.len(),
        1,
        "VS Code section must contain exactly one json settings snippet"
    );

    let settings: Value = serde_json::from_str(blocks[0])
        .expect("VS Code settings snippet must be valid, copy-paste-able JSON");
    let rendered = settings.to_string();
    assert!(
        rendered.contains(BINARY),
        "VS Code snippet must launch the `{BINARY}` binary"
    );
    assert!(
        rendered.contains("plaintext"),
        "VS Code snippet must register the server for plaintext documents"
    );
}

#[test]
fn zed_section_explains_extension_requirement() {
    let zed = section(README, "Zed");
    assert!(
        zed.contains("language extension"),
        "Zed section must explain that a language extension is required"
    );
    assert!(
        zed.contains("cannot register a new arbitrary server"),
        "Zed section must not imply that settings alone can register the server"
    );
    assert!(
        zed.contains("https://zed.dev/docs/extensions/languages"),
        "Zed section must link to the language extension documentation"
    );
}

#[test]
fn editor_setup_has_troubleshooting_subsection() {
    let setup = section(README, "Editor setup");
    assert!(
        setup.contains("### Troubleshooting"),
        "Editor setup section must include a Troubleshooting subsection"
    );
}
