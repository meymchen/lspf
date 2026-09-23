//! Hover on a link destination: the target heading and URI, or an image
//! preview.

use std::sync::Arc;

use lspf::types::{Contents, Hover, HoverParams, MarkupContent, MarkupKind};
use lspf::{CancellationToken, LspError, ServerContext};

use crate::State;
use crate::features::{Located, locate, located_href};
use crate::target::is_external;

pub(crate) async fn hover(
    state: Arc<State>,
    ctx: ServerContext,
    params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let position = params.text_document_position_params;
    let uri = position.text_document.uri;
    let Some(entry) = state.index.get(&ctx, &uri).await else {
        return Ok(None);
    };
    let Some(offset) = entry.offset(position.position) else {
        return Ok(None);
    };
    let Some(located) = locate(&entry, offset) else {
        return Ok(None);
    };
    let image = matches!(located, Located::Link(link) if link.image)
        || matches!(located, Located::Reference(reference) if reference.image);
    let Some(href) = located_href(&entry, &located) else {
        return Ok(None);
    };
    if is_external(&href.text) {
        return Ok(None);
    }
    let Some(resolved) = state.index.resolve(&ctx, &uri, &href.text).await else {
        return Ok(None);
    };
    let range = entry.range(&href.range);
    let target = resolved.target;

    if image {
        if resolved.kind.is_none() {
            return Ok(None);
        }
        return Ok(Some(Hover {
            contents: Contents::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("![]({})", target.uri.as_str()),
            }),
            range,
        }));
    }

    let Some(target_entry) = state.index.get(&ctx, &target.uri).await else {
        return Ok(None);
    };
    let heading = match target.fragment.as_deref() {
        Some(fragment) => target_entry.md.heading_for_fragment(fragment),
        None => target_entry.md.headings.first(),
    };
    let title = heading.map_or_else(|| target.display(), |heading| heading.title.clone());

    Ok(Some(Hover {
        contents: Contents::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!(
                "**{title}**\n\n{tick}{}{tick}",
                target.display(),
                tick = char::from(96)
            ),
        }),
        range,
    }))
}
