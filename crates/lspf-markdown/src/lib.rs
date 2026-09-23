//! First-party Markdown language server built on lspf.
//!
//! Modeled on `vscode-markdown-languageservice`, it provides:
//!
//! - link diagnostics: missing files and headings, undefined references, and
//!   duplicate or unused link definitions;
//! - hover, go to definition, and document links for link destinations;
//! - document and workspace symbols, folding ranges, and smart selection;
//! - find references, document highlights, and rename for headings, reference
//!   labels, and linked files;
//! - completion of link paths, heading fragments, and reference labels;
//! - code actions that organize or extract link definitions;
//! - link updates when files or directories are renamed.

mod features;
mod fs;
mod index;
mod parse;
mod slug;
mod target;

use std::borrow::Cow;
use std::sync::Arc;

use lspf::types::{
    CodeActionKind, CodeActionOptions, CompletionOptions, DefinitionOptions,
    DidChangeTextDocumentNotification as DidChangeTextDocument, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidCloseTextDocumentNotification as DidCloseTextDocument,
    DidCloseTextDocumentParams, DidOpenTextDocumentNotification as DidOpenTextDocument,
    DidOpenTextDocumentParams, DocumentLinkOptions, FileChangeType, FileOperationFilter,
    FileOperationPattern, FileOperationRegistrationOptions, InitializedParams, Registration,
    RegistrationParams, RenameFilesParams, RenameOptions,
};
use lspf::{Server, ServerContext};
use serde_json::json;

pub use fs::{FileKind, MemoryFs, OsFs, WorkspaceFs};

use features::code_actions::ORGANIZE_LINK_DEFINITIONS;
use features::diagnostics;
use index::{MARKDOWN_EXTENSIONS, WorkspaceIndex};

/// Application state for the Markdown server.
pub struct State {
    index: WorkspaceIndex,
}

async fn did_open(state: Arc<State>, ctx: ServerContext, params: DidOpenTextDocumentParams) {
    let uri = params.text_document.uri;
    state.index.opened(&uri);
    diagnostics::publish(&state, &ctx, uri).await;
}

async fn did_change(state: Arc<State>, ctx: ServerContext, params: DidChangeTextDocumentParams) {
    diagnostics::publish(
        &state,
        &ctx,
        params.text_document.text_document_identifier.uri,
    )
    .await;
}

async fn did_close(state: Arc<State>, _ctx: ServerContext, params: DidCloseTextDocumentParams) {
    state.index.closed(&params.text_document.uri);
}

async fn did_change_watched_files(
    state: Arc<State>,
    ctx: ServerContext,
    params: DidChangeWatchedFilesParams,
) {
    for change in &params.changes {
        let listing_changed = !matches!(change.kind, FileChangeType::Changed);
        state.index.changed(&change.uri, listing_changed);
    }
    diagnostics::publish_all(&state, &ctx).await;
}

async fn did_rename_files(state: Arc<State>, ctx: ServerContext, _params: RenameFilesParams) {
    state.index.reset();
    diagnostics::publish_all(&state, &ctx).await;
}

fn supports_watch_registration(ctx: &ServerContext) -> bool {
    ctx.workspace()
        .capabilities()
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.did_change_watched_files.as_ref())
        .and_then(|watched| watched.dynamic_registration)
        .unwrap_or(false)
}

async fn initialized(state: Arc<State>, ctx: ServerContext, _params: InitializedParams) {
    if !supports_watch_registration(&ctx) {
        return;
    }
    // The engine awaits this hook before reading the client's next message,
    // so the registration round trip runs in its own task.
    tokio::spawn(async move {
        let glob = format!("**/*.{{{}}}", MARKDOWN_EXTENSIONS.join(","));
        let registered = ctx
            .client()
            .register_capability(RegistrationParams {
                registrations: vec![Registration {
                    id: "lspf-markdown-watch".to_string(),
                    method: "workspace/didChangeWatchedFiles".to_string(),
                    register_options: Some(json!({ "watchers": [{ "globPattern": glob }] })),
                }],
            })
            .await;
        if registered.is_ok() {
            state.index.watching();
        }
    });
}

fn all_files() -> FileOperationRegistrationOptions {
    FileOperationRegistrationOptions {
        filters: vec![FileOperationFilter {
            scheme: Some("file".to_string()),
            pattern: FileOperationPattern {
                glob: "**/*".to_string(),
                matches: None,
                options: None,
            },
        }],
    }
}

/// Build a Markdown server over the caller's workspace file system.
pub fn server(fs: impl WorkspaceFs) -> Server<State> {
    let state = State {
        index: WorkspaceIndex::new(Arc::new(fs.clone())),
    };
    Server::builder(state)
        .file_provider(fs)
        .feature(lspf::features::hover(), features::hover::hover)
        .feature(
            lspf::features::definition(DefinitionOptions {
                work_done_progress_options: Default::default(),
            }),
            features::definition::definition,
        )
        .feature(
            lspf::features::document_symbol(Default::default()),
            features::symbols::document_symbols,
        )
        .feature(
            lspf::features::workspace_symbol(Default::default()),
            features::symbols::workspace_symbols,
        )
        .feature(
            lspf::features::folding_range(Default::default()),
            features::folding::folding,
        )
        .feature(
            lspf::features::selection_range(Default::default()),
            features::selection::selection_ranges,
        )
        .feature(
            lspf::features::document_link(DocumentLinkOptions {
                resolve_provider: Some(true),
                ..Default::default()
            }),
            features::links::document_links,
        )
        .feature(
            lspf::features::document_link_resolve(),
            features::links::resolve_document_link,
        )
        .feature(
            lspf::features::references(Default::default()),
            features::references::references,
        )
        .feature(
            lspf::features::document_highlight(Default::default()),
            features::references::document_highlights,
        )
        .feature(
            lspf::features::rename(RenameOptions {
                prepare_provider: Some(true),
                ..Default::default()
            }),
            features::rename::rename,
        )
        .feature(
            lspf::features::prepare_rename(),
            features::rename::prepare_rename,
        )
        .feature(
            lspf::features::completion(CompletionOptions {
                trigger_characters: Some([".", "/", "#", "["].map(ToString::to_string).to_vec()),
                ..Default::default()
            }),
            features::completion::completion,
        )
        .feature(
            lspf::features::code_action(CodeActionOptions {
                code_action_kinds: Some(vec![
                    CodeActionKind::QuickFix,
                    CodeActionKind::RefactorExtract,
                    CodeActionKind::Custom(Cow::Borrowed(ORGANIZE_LINK_DEFINITIONS)),
                ]),
                ..Default::default()
            }),
            features::code_actions::code_actions,
        )
        .feature(
            lspf::features::will_rename_files(all_files()),
            features::file_rename::will_rename_files,
        )
        .feature_notification(
            lspf::features::did_rename_files(all_files()),
            did_rename_files,
        )
        .feature_notification(
            lspf::features::did_change_watched_files(),
            did_change_watched_files,
        )
        .notification::<DidOpenTextDocument, _, _>(did_open)
        .notification::<DidChangeTextDocument, _, _>(did_change)
        .notification::<DidCloseTextDocument, _, _>(did_close)
        .on_initialized(initialized)
        .build()
        .expect("valid registrations")
}
