//! Document links. External destinations carry their target directly; local
//! ones resolve lazily, so opening a document never stats every target.

use std::str::FromStr;
use std::sync::Arc;

use lspf::types::{DocumentLink, DocumentLinkParams, Uri};
use lspf::{CancellationToken, LspError, ServerContext};
use serde_json::{Value, json};

use crate::State;
use crate::features::hrefs;
use crate::target::is_external;

pub(crate) async fn document_links(
    state: Arc<State>,
    ctx: ServerContext,
    params: DocumentLinkParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<DocumentLink>>, LspError> {
    let uri = params.text_document.uri;
    let Some(entry) = state.index.get(&ctx, &uri).await else {
        return Ok(None);
    };
    let mut links = Vec::new();
    let mut add = |range: &std::ops::Range<usize>, href: &str| {
        let Some(range) = entry.range(range) else {
            return;
        };
        if is_external(href) {
            if let Ok(target) = Uri::from_str(href) {
                links.push(DocumentLink {
                    range,
                    target: Some(target),
                    ..DocumentLink::default()
                });
            }
            return;
        }
        links.push(DocumentLink {
            range,
            data: Some(json!({ "source": uri.as_str(), "href": href })),
            ..DocumentLink::default()
        });
    };
    for href in hrefs(&entry) {
        add(&href.range, &href.text);
    }
    for reference in &entry.md.references {
        if let Some(definition) = entry.md.definition(&reference.label) {
            add(&reference.label_range, &definition.dest.clone());
        }
    }
    links.sort_by_key(|link| (link.range.start.line, link.range.start.character));
    Ok(Some(links))
}

fn line_fragment(uri: &Uri, line: u32, character: u32) -> Option<Uri> {
    Uri::from_str(&format!(
        "{}#L{},{}",
        crate::target::without_fragment(uri.as_str()),
        line + 1,
        character + 1
    ))
    .ok()
}

pub(crate) async fn resolve_document_link(
    state: Arc<State>,
    ctx: ServerContext,
    mut link: DocumentLink,
    _ct: CancellationToken,
) -> Result<DocumentLink, LspError> {
    let Some(Value::Object(data)) = link.data.take() else {
        return Ok(link);
    };
    let (Some(source), Some(href)) = (
        data.get("source").and_then(Value::as_str),
        data.get("href").and_then(Value::as_str),
    ) else {
        return Ok(link);
    };
    let Ok(source) = Uri::from_str(source) else {
        return Ok(link);
    };
    let Some(resolved) = state.index.resolve(&ctx, &source, href).await else {
        return Ok(link);
    };
    let target = resolved.target;
    let heading_position = match target.fragment.as_deref() {
        Some(fragment) => state.index.get(&ctx, &target.uri).await.and_then(|entry| {
            let heading = entry.md.heading_for_fragment(fragment)?;
            entry.position(heading.range.start)
        }),
        None => None,
    };
    link.target = match (heading_position, target.fragment.as_deref()) {
        (Some(position), _) => line_fragment(&target.uri, position.line, position.character),
        (None, Some(fragment)) => Uri::from_str(&format!("{}#{fragment}", target.uri.as_str()))
            .ok()
            .or(Some(target.uri)),
        (None, None) => Some(target.uri),
    };
    Ok(link)
}
