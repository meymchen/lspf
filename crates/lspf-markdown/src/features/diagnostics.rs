//! Pushed link diagnostics.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use lspf::ServerContext;
use lspf::types::{Diagnostic, DiagnosticSeverity, DiagnosticTag, PublishDiagnosticsParams, Uri};

use crate::State;
use crate::fs::FileKind;
use crate::index::{Entry, is_markdown_path};
use crate::parse::normalize_label;
use crate::target::is_external;

pub(crate) const SOURCE: &str = "lspf-markdown";
pub(crate) const NO_SUCH_REFERENCE: &str = "link.no-such-reference";
pub(crate) const DUPLICATE_DEFINITION: &str = "link.duplicate-definition";
pub(crate) const UNUSED_DEFINITION: &str = "link.unused-definition";

/// Whether a fragment addresses a line (`#L12`, `#L12,3`) rather than a
/// heading. Line fragments are editor navigation, not anchors.
fn is_line_fragment(fragment: &str) -> bool {
    fragment
        .strip_prefix('L')
        .is_some_and(|rest| rest.split(',').all(|part| part.parse::<u32>().is_ok()))
}

enum Problem {
    MissingTarget,
    MissingHeading,
}

async fn check_href(
    state: &State,
    ctx: &ServerContext,
    source: &Uri,
    href: &str,
) -> Option<Problem> {
    if is_external(href) {
        return None;
    }
    let resolved = state.index.resolve(ctx, source, href).await?;
    match resolved.kind {
        None => Some(Problem::MissingTarget),
        Some(FileKind::Directory) => None,
        Some(FileKind::File) => {
            let fragment = resolved.target.fragment.as_deref()?;
            if is_line_fragment(fragment)
                || !(is_markdown_path(&resolved.target.uri)
                    || state.index.is_open(&resolved.target.uri))
            {
                return None;
            }
            let target = state.index.get(ctx, &resolved.target.uri).await?;
            target
                .md
                .heading_for_fragment(fragment)
                .is_none()
                .then_some(Problem::MissingHeading)
        }
    }
}

fn diagnostic(
    entry: &Entry,
    range: &Range<usize>,
    severity: DiagnosticSeverity,
    code: Option<&str>,
    message: String,
) -> Option<Diagnostic> {
    Some(Diagnostic {
        range: entry.range(range)?,
        severity: Some(severity),
        code: code.map(Into::into),
        source: Some(SOURCE.into()),
        message: message.into(),
        ..Diagnostic::default()
    })
}

/// Compute every diagnostic for one parsed document.
pub(crate) async fn compute(state: &State, ctx: &ServerContext, entry: &Entry) -> Vec<Diagnostic> {
    let uri = entry.uri();
    let md = &entry.md;
    let mut located: Vec<(usize, Diagnostic)> = Vec::new();

    let mut targets: Vec<(Range<usize>, String)> = md
        .links
        .iter()
        .map(|link| (link.href_range.clone(), link.href.clone()))
        .collect();
    targets.extend(md.references.iter().filter_map(|reference| {
        let definition = md.definition(&reference.label)?;
        Some((reference.label_range.clone(), definition.dest.clone()))
    }));
    for (range, href) in targets {
        let message = match check_href(state, ctx, uri, &href).await {
            Some(Problem::MissingTarget) => format!("local link target does not exist: {href}"),
            Some(Problem::MissingHeading) => format!("local link heading does not exist: {href}"),
            None => continue,
        };
        if let Some(found) = diagnostic(entry, &range, DiagnosticSeverity::Error, None, message) {
            located.push((range.start, found));
        }
    }

    for broken in &md.broken_references {
        if let Some(found) = diagnostic(
            entry,
            &broken.label_range,
            DiagnosticSeverity::Error,
            Some(NO_SUCH_REFERENCE),
            format!("no link definition found: '{}'", broken.label),
        ) {
            located.push((broken.label_range.start, found));
        }
    }

    let used: HashSet<String> = md
        .references
        .iter()
        .map(|reference| normalize_label(&reference.label))
        .collect();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for definition in &md.definitions {
        let label = normalize_label(&definition.label);
        let duplicate = seen.insert(label.clone(), definition.range.start).is_some();
        let found = if duplicate {
            diagnostic(
                entry,
                &definition.label_range,
                DiagnosticSeverity::Warning,
                Some(DUPLICATE_DEFINITION),
                format!("link definition for '{}' already exists", definition.label),
            )
        } else if !used.contains(&label) {
            diagnostic(
                entry,
                &definition.range,
                DiagnosticSeverity::Hint,
                Some(UNUSED_DEFINITION),
                "link definition is unused".to_string(),
            )
            .map(|mut found| {
                found.tags = Some(vec![DiagnosticTag::Unnecessary]);
                found
            })
        } else {
            None
        };
        if let Some(found) = found {
            located.push((definition.range.start, found));
        }
    }

    located.sort_by_key(|(start, _)| *start);
    located.into_iter().map(|(_, found)| found).collect()
}

/// Publish the diagnostics of the open document at `uri`.
pub(crate) async fn publish(state: &State, ctx: &ServerContext, uri: Uri) {
    let Some(document) = ctx.documents().get(&uri) else {
        return;
    };
    let Some(entry) = state.index.get(ctx, &uri).await else {
        return;
    };
    let diagnostics = compute(state, ctx, &entry).await;
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: document.version(),
    };
    let _ = ctx.client().publish_diagnostics(params);
}

/// Republish every open document, after a change elsewhere in the workspace
/// may have repaired or broken its links.
pub(crate) async fn publish_all(state: &State, ctx: &ServerContext) {
    for uri in state.index.open_uris() {
        publish(state, ctx, uri).await;
    }
}

#[cfg(test)]
mod tests {
    use super::is_line_fragment;

    #[test]
    fn line_fragments_are_recognized() {
        assert!(is_line_fragment("L12"));
        assert!(is_line_fragment("L12,3"));
        assert!(!is_line_fragment("Lines"));
        assert!(!is_line_fragment("install"));
    }
}
