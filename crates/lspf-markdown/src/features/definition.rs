//! Go to definition: a link navigates to its target heading; a reference
//! whose destination is external navigates to its link definition.

use std::sync::Arc;

use lspf::types::{Definition, DefinitionParams, DefinitionResponse, Location, Position, Range};
use lspf::{CancellationToken, LspError, ServerContext};

use crate::State;
use crate::features::{Located, locate, located_href};
use crate::target::is_external;

fn location(uri: lspf::types::Uri, range: Range) -> DefinitionResponse {
    DefinitionResponse::Definition(Definition::Location(Location { uri, range }))
}

pub(crate) async fn definition(
    state: Arc<State>,
    ctx: ServerContext,
    params: DefinitionParams,
    _ct: CancellationToken,
) -> Result<Option<DefinitionResponse>, LspError> {
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
    let Some(href) = located_href(&entry, &located) else {
        return Ok(None);
    };
    if is_external(&href.text) {
        let Located::Reference(reference) = located else {
            return Ok(None);
        };
        let Some(definition) = entry.md.definition(&reference.label) else {
            return Ok(None);
        };
        return Ok(entry
            .range(&definition.label_range)
            .map(|range| location(uri, range)));
    }
    let Some(resolved) = state.index.resolve(&ctx, &uri, &href.text).await else {
        return Ok(None);
    };
    let target = resolved.target;
    let Some(target_entry) = state.index.get(&ctx, &target.uri).await else {
        return Ok(None);
    };
    let heading = match target.fragment.as_deref() {
        Some(fragment) => target_entry.md.heading_for_fragment(fragment),
        None => target_entry.md.headings.first(),
    };
    let range = heading
        .and_then(|heading| target_entry.range(&heading.content))
        .unwrap_or_else(|| Range::new(Position::new(0, 0), Position::new(0, 0)));
    Ok(Some(location(target.uri, range)))
}
