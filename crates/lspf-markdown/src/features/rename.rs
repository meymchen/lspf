//! Rename headings, reference labels, and linked files.

use std::collections::HashMap;
use std::sync::Arc;

use lspf::types::{
    DocumentChange, Edit, OptionalVersionedTextDocumentIdentifier, PrepareRenameParams,
    PrepareRenamePlaceholder, PrepareRenameResult, RenameFile, RenameParams, ResourceOperationKind,
    TextDocumentEdit, TextDocumentIdentifier, TextEdit, Uri, WorkspaceEdit,
};
use lspf::{CancellationToken, LspError, ServerContext};

use crate::State;
use crate::index::WorkspaceIndex;
use crate::link_resolution::{LinkEdit, LinkResolution, Search, Symbol};
use crate::slug::slugify;
use crate::target::{is_external, resolve_local_target, uri_key};

/// Text edits grouped by document, in first-touched order.
#[derive(Default)]
pub(crate) struct Edits {
    documents: Vec<(Uri, Vec<TextEdit>)>,
    index: HashMap<String, usize>,
}

impl Edits {
    pub(crate) fn push(&mut self, uri: &Uri, edit: TextEdit) {
        let key = uri_key(uri);
        let slot = *self.index.entry(key).or_insert_with(|| {
            self.documents.push((uri.clone(), Vec::new()));
            self.documents.len() - 1
        });
        let edits = &mut self.documents[slot].1;
        if !edits.iter().any(|existing| existing.range == edit.range) {
            edits.push(edit);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }

    /// Build a workspace edit; file renames force the `documentChanges` form
    /// and run after the text edits that address the old URIs.
    pub(crate) fn into_workspace_edit(self, renames: Vec<RenameFile>) -> WorkspaceEdit {
        if renames.is_empty() {
            return WorkspaceEdit {
                changes: Some(self.documents.into_iter().collect()),
                ..WorkspaceEdit::default()
            };
        }
        let mut changes: Vec<DocumentChange> = self
            .documents
            .into_iter()
            .map(|(uri, edits)| {
                DocumentChange::TextDocumentEdit(TextDocumentEdit {
                    text_document: OptionalVersionedTextDocumentIdentifier {
                        version: None,
                        text_document_identifier: TextDocumentIdentifier { uri },
                    },
                    edits: edits.into_iter().map(Edit::TextEdit).collect(),
                })
            })
            .collect();
        changes.extend(renames.into_iter().map(DocumentChange::RenameFile));
        WorkspaceEdit {
            document_changes: Some(changes),
            ..WorkspaceEdit::default()
        }
    }
}

impl FromIterator<LinkEdit> for Edits {
    fn from_iter<I: IntoIterator<Item = LinkEdit>>(links: I) -> Self {
        let mut edits = Self::default();
        for link in links {
            if let Some(range) = link.entry.range(&link.range) {
                edits.push(
                    link.entry.uri(),
                    TextEdit {
                        range,
                        new_text: link.new_text,
                    },
                );
            }
        }
        edits
    }
}

fn not_here() -> LspError {
    LspError::RequestFailed("renaming is not supported here".to_string())
}

pub(crate) async fn prepare_rename(
    state: Arc<State>,
    ctx: ServerContext,
    params: PrepareRenameParams,
    _ct: CancellationToken,
) -> Result<Option<PrepareRenameResult>, LspError> {
    let position = params.text_document_position_params;
    let entry = state
        .index
        .get(&ctx, &position.text_document.uri)
        .await
        .ok_or_else(not_here)?;
    let offset = entry.offset(position.position).ok_or_else(not_here)?;
    let mut links = LinkResolution::new(&state.index, &ctx);
    let selected = links
        .select(Arc::clone(&entry), offset, Search::CursorOnly)
        .await
        .ok_or_else(not_here)?;
    require_existing_target(&selected.symbol)?;
    let range = selected.range;
    let text = entry.text();
    Ok(Some(PrepareRenameResult::PrepareRenamePlaceholder(
        PrepareRenamePlaceholder {
            range: entry.range(&range).ok_or_else(not_here)?,
            placeholder: text[range].to_string(),
        },
    )))
}

fn require_existing_target(symbol: &Symbol) -> Result<(), LspError> {
    if matches!(symbol, Symbol::File { target } if target.kind.is_none()) {
        return Err(LspError::RequestFailed(
            "the link target does not exist".to_string(),
        ));
    }
    Ok(())
}

fn supports_file_rename(ctx: &ServerContext) -> bool {
    ctx.workspace()
        .capabilities()
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.workspace_edit.as_ref())
        .is_some_and(|edit| {
            edit.document_changes == Some(true)
                && edit
                    .resource_operations
                    .as_ref()
                    .is_some_and(|operations| operations.contains(&ResourceOperationKind::Rename))
        })
}

pub(crate) async fn rename(
    state: Arc<State>,
    ctx: ServerContext,
    params: RenameParams,
    _ct: CancellationToken,
) -> Result<Option<WorkspaceEdit>, LspError> {
    let position = params.text_document_position_params;
    let new_name = params.new_name;
    let entry = state
        .index
        .get(&ctx, &position.text_document.uri)
        .await
        .ok_or_else(not_here)?;
    let offset = entry.offset(position.position).ok_or_else(not_here)?;
    let mut links = LinkResolution::new(&state.index, &ctx);
    let selected = links
        .select(Arc::clone(&entry), offset, Search::Rename)
        .await
        .ok_or_else(not_here)?;
    require_existing_target(&selected.symbol)?;
    let symbol = selected.symbol;

    let mut edits = Edits::default();
    let mut renames = Vec::new();
    match &symbol {
        Symbol::Label { .. } | Symbol::Heading { .. } => {
            let new_slug = slugify(&new_name);
            for occurrence in selected.occurrences {
                let new_text = match (&symbol, occurrence.declaration) {
                    (Symbol::Heading { .. }, false) => new_slug.clone(),
                    _ => new_name.clone(),
                };
                if let Some(range) = occurrence.entry.range(&occurrence.range) {
                    edits.push(occurrence.entry.uri(), TextEdit { range, new_text });
                }
            }
        }
        Symbol::File { target } => {
            let old = &target.target.uri;
            if !supports_file_rename(&ctx) {
                return Err(LspError::RequestFailed(
                    "the client cannot rename files".to_string(),
                ));
            }
            if is_external(&new_name) {
                return Err(not_here());
            }
            let root = WorkspaceIndex::root_for(&ctx, entry.uri());
            let new = resolve_local_target(entry.uri(), &new_name, root.as_ref())
                .ok_or_else(not_here)?
                .uri;
            let new = if crate::target::file_name(&new).contains('.')
                || !crate::index::is_markdown_path(old)
            {
                new
            } else {
                let extension = crate::target::file_name(old)
                    .rsplit_once('.')
                    .map(|(_, extension)| extension.to_string())
                    .unwrap_or_default();
                format!("{}.{extension}", new.as_str())
                    .parse()
                    .map_err(|_| not_here())?
            };
            if uri_key(&new) == uri_key(old) {
                return Ok(None);
            }
            edits = links
                .edits_for_moves(&[(old.clone(), new.clone())])
                .await
                .into_iter()
                .collect();
            renames.push(RenameFile {
                old_uri: old.clone(),
                new_uri: new,
                options: None,
                annotation_id: None,
            });
        }
    }
    if edits.is_empty() && renames.is_empty() {
        return Ok(None);
    }
    Ok(Some(edits.into_workspace_edit(renames)))
}
