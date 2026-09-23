//! Link updates when files or directories move.

use std::sync::Arc;

use lspf::types::{RenameFilesParams, Uri, WorkspaceEdit};
use lspf::{CancellationToken, LspError, ServerContext};

use crate::State;
use crate::features::rename::Edits;
use crate::link_resolution::LinkResolution;

pub(crate) async fn will_rename_files(
    state: Arc<State>,
    ctx: ServerContext,
    params: RenameFilesParams,
    _ct: CancellationToken,
) -> Result<Option<WorkspaceEdit>, LspError> {
    let renames: Vec<(Uri, Uri)> = params
        .files
        .into_iter()
        .map(|file| (file.old_uri, file.new_uri))
        .collect();
    let mut links = LinkResolution::new(&state.index, &ctx);
    let edits: Edits = links.edits_for_moves(&renames).await.into_iter().collect();
    Ok((!edits.is_empty()).then(|| edits.into_workspace_edit(Vec::new())))
}
