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
use crate::features::file_rename::link_edits;
use crate::features::references::{Symbol, occurrences, scope, symbol_at};
use crate::features::{Located, locate, located_href};
use crate::index::WorkspaceIndex;
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
    symbol_at(&state, &ctx, &entry, offset)
        .await
        .ok_or_else(not_here)?;
    let located = locate(&entry, offset).ok_or_else(not_here)?;
    let range = match &located {
        Located::Heading(heading) => heading.content.clone(),
        Located::Reference(reference) => reference.label_range.clone(),
        Located::DefinitionLabel(definition) => definition.label_range.clone(),
        Located::Link(_) | Located::DefinitionDest(_) => {
            let href = located_href(&entry, &located).ok_or_else(not_here)?;
            match href.fragment_range() {
                Some(fragment) if offset >= fragment.start || href.path_range().is_empty() => {
                    fragment
                }
                _ => href.path_range(),
            }
        }
    };
    let text = entry.text();
    Ok(Some(PrepareRenameResult::PrepareRenamePlaceholder(
        PrepareRenamePlaceholder {
            range: entry.range(&range).ok_or_else(not_here)?,
            placeholder: text[range].to_string(),
        },
    )))
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
    let symbol = symbol_at(&state, &ctx, &entry, offset)
        .await
        .ok_or_else(not_here)?;

    let mut edits = Edits::default();
    let mut renames = Vec::new();
    match &symbol {
        Symbol::Label { .. } | Symbol::Heading { .. } => {
            let new_slug = slugify(&new_name);
            let entries = scope(&state, &ctx, &symbol).await;
            for occurrence in occurrences(&state, &ctx, &symbol, &entries).await {
                let new_text = match (&symbol, occurrence.declaration) {
                    (Symbol::Heading { .. }, false) => new_slug.clone(),
                    _ => new_name.clone(),
                };
                if let Some(range) = occurrence.entry.range(&occurrence.range) {
                    edits.push(occurrence.entry.uri(), TextEdit { range, new_text });
                }
            }
        }
        Symbol::File { uri: old } => {
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
            edits = link_edits(&state, &ctx, &[(old.clone(), new.clone())]).await;
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
