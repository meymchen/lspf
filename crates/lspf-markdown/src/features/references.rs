//! References and highlights over the shared Markdown link interpretation.

use std::sync::Arc;

use lspf::types::{
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams, Location, ReferenceParams,
};
use lspf::{CancellationToken, LspError, ServerContext};

use crate::State;
use crate::link_resolution::{LinkResolution, Search};

pub(crate) async fn references(
    state: Arc<State>,
    ctx: ServerContext,
    params: ReferenceParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<Location>>, LspError> {
    let position = params.text_document_position_params;
    let Some(entry) = state.index.get(&ctx, &position.text_document.uri).await else {
        return Ok(None);
    };
    let Some(offset) = entry.offset(position.position) else {
        return Ok(None);
    };
    let mut links = LinkResolution::new(&state.index, &ctx);
    let Some(selected) = links.select(entry, offset, Search::Workspace).await else {
        return Ok(None);
    };
    let locations = selected
        .occurrences
        .into_iter()
        .filter(|occurrence| params.context.include_declaration || !occurrence.declaration)
        .filter_map(|occurrence| occurrence.location())
        .collect();
    Ok(Some(locations))
}

pub(crate) async fn document_highlights(
    state: Arc<State>,
    ctx: ServerContext,
    params: DocumentHighlightParams,
    _ct: CancellationToken,
) -> Result<Option<Vec<DocumentHighlight>>, LspError> {
    let position = params.text_document_position_params;
    let Some(entry) = state.index.get(&ctx, &position.text_document.uri).await else {
        return Ok(None);
    };
    let Some(offset) = entry.offset(position.position) else {
        return Ok(None);
    };
    let mut links = LinkResolution::new(&state.index, &ctx);
    let Some(selected) = links.select(entry, offset, Search::Document).await else {
        return Ok(None);
    };
    let highlights = selected
        .occurrences
        .into_iter()
        .filter_map(|occurrence| {
            Some(DocumentHighlight {
                range: occurrence.entry.range(&occurrence.range)?,
                kind: Some(if occurrence.declaration {
                    DocumentHighlightKind::Write
                } else {
                    DocumentHighlightKind::Read
                }),
            })
        })
        .collect();
    Ok(Some(highlights))
}
