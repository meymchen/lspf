//! The template language server: a built `Server` served over stdio.
//!
//! It demonstrates the complete typed journey the framework supports with no
//! handwritten `ServerCapabilities` field and no framework modification — the
//! registrations are the capabilities:
//!
//! - standard features registered through sealed descriptors — hover,
//!   completion, and the dependent completion resolve;
//! - two typed Commands dispatched beneath `workspace/executeCommand`, one of
//!   which reads multi-root workspace state and one of which reads a file
//!   that is not open in the editor;
//! - a post-mutation hook for `textDocument/didOpen`, observing the document
//!   the framework has already opened;
//! - an `OsFileProvider` configured on the builder, so unopened `file:` URIs
//!   resolve from disk.
//!
//! Fork it as the starting point for a real server.

use std::str::FromStr;
use std::sync::Arc;

use lspf::types::notification::{DidOpenTextDocument, PublishDiagnostics};
use lspf::types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidOpenTextDocumentParams, Hover, HoverContents, HoverParams,
    MarkedString, Position, PublishDiagnosticsParams, Range, Uri,
};
use lspf::{CancellationToken, Context, LspError, OsFileProvider, Server};
use tracing::{debug, warn};

/// This server's own application state, shared by every handler as `Arc<State>`.
///
/// The framework's documents, workspace, and client are reached through the
/// `Context` parameter, never stored here; a real server would keep its
/// analysis results, caches, or configuration in this struct instead. This one
/// has none, so it is empty.
struct State;

impl State {
    fn new() -> Self {
        Self
    }
}

/// The post-mutation hook for the built-in `textDocument/didOpen`.
///
/// The protocol engine has already decoded the notification and opened the
/// document by the time this runs, so the hook observes the retained
/// [`Document`](lspf::Document) through `ctx.documents()` rather than trusting
/// the wire parameters, and reports on the state every later handler will see.
async fn on_did_open(_state: Arc<State>, ctx: Context, params: DidOpenTextDocumentParams) {
    let uri = params.text_document.uri;
    let Some(document) = ctx.documents().get(&uri) else {
        // The built-in mutation runs before the hook, so a missing document
        // means the notification never reached it — there is nothing to report.
        warn!(?uri, "didOpen hook found no open document");
        return;
    };
    debug!(?uri, "publishing the open diagnostic");

    let diagnostics = PublishDiagnosticsParams {
        uri,
        // The retained version, not the one on the wire: that is the revision
        // this diagnostic actually describes.
        version: document.version(),
        diagnostics: vec![Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 0,
                },
            },
            severity: Some(DiagnosticSeverity::INFORMATION),
            source: Some("lspf-hello".into()),
            message: "lspf saw this document open".into(),
            ..Diagnostic::default()
        }],
    };
    // A notification is fire-and-forget: it is encoded and enqueued
    // synchronously, and a closing connection is the only way it fails.
    if let Err(error) = ctx.client().notify::<PublishDiagnostics>(diagnostics) {
        warn!(%error, "publishing the open diagnostic failed");
    }
}

/// The typed `textDocument/hover` feature: registered through the sealed
/// [`hover`](lspf::features::hover) descriptor, which contributes
/// `hoverProvider: true` to the generated capabilities and fixes this
/// handler's parameter and result types.
///
/// The handler reads the same framework-owned document every other handler
/// sees — the engine applied `didOpen` and any incremental `didChange` before
/// this request was dispatched.
async fn hover(
    _state: Arc<State>,
    ctx: Context,
    params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let uri = &params.text_document_position_params.text_document.uri;
    let Some(document) = ctx.documents().get(uri) else {
        return Ok(None);
    };
    let words = document.text().split_whitespace().count();
    Ok(Some(Hover {
        contents: HoverContents::Scalar(MarkedString::String(format!(
            "`{}` · {words} words · version {:?}",
            document.language_id(),
            document.version(),
        ))),
        range: None,
    }))
}

/// The typed `textDocument/completion` feature: the
/// [`completion(options)`](lspf::features::completion) descriptor advertises
/// exactly the supplied options as `completionProvider`.
async fn completion(
    _state: Arc<State>,
    _ctx: Context,
    _params: CompletionParams,
    _ct: CancellationToken,
) -> Result<Option<CompletionResponse>, LspError> {
    Ok(Some(CompletionResponse::Array(vec![
        CompletionItem {
            label: "lspf-hello".into(),
            kind: Some(CompletionItemKind::TEXT),
            detail: Some("a template server".into()),
            ..CompletionItem::default()
        },
        CompletionItem {
            label: "workspaceRoots".into(),
            kind: Some(CompletionItemKind::METHOD),
            detail: Some("read the multi-root workspace state".into()),
            ..CompletionItem::default()
        },
    ])))
}

/// The dependent `completionItem/resolve` feature. Registering it alongside
/// [`completion`] is what turns the advertised completion provider into an
/// options object carrying `resolveProvider: true`; registering it alone
/// would fail the build with a dangling `resolveProvider`.
async fn resolve_completion(
    _state: Arc<State>,
    _ctx: Context,
    item: CompletionItem,
    _ct: CancellationToken,
) -> Result<CompletionItem, LspError> {
    let mut resolved = item;
    if resolved.detail.is_none() {
        resolved.detail = Some("resolved by lspf-hello".into());
    }
    Ok(resolved)
}

/// A typed Command reading the connection's live multi-root workspace state.
///
/// The engine routes `workspace/executeCommand` here by name, and the
/// registration contributes `lspf-hello.workspaceRoots` to the generated
/// `executeCommandProvider` (in registration order, ADR 0022).
async fn workspace_roots(
    _state: Arc<State>,
    ctx: Context,
    _args: Vec<String>,
    _ct: CancellationToken,
) -> Result<Vec<(String, String)>, LspError> {
    Ok(ctx
        .workspace()
        .roots()
        .into_iter()
        .map(|folder| (folder.uri.as_str().to_string(), folder.name))
        .collect())
}

/// A typed Command reading a file that is not open in the editor.
///
/// `ctx.workspace().text_document` prefers editor-open text and falls back to
/// the connection's configured [`FileProvider`](lspf::FileProvider) — here the
/// `OsFileProvider` configured on the builder — so a URI the server has never
/// seen still resolves.
async fn read_file(
    _state: Arc<State>,
    ctx: Context,
    args: Vec<String>,
    _ct: CancellationToken,
) -> Result<String, LspError> {
    let Some(arg) = args.into_iter().next() else {
        return Err(LspError::invalid_params(
            "lspf-hello.readFile expects one file URI argument",
        ));
    };
    let uri = Uri::from_str(&arg)
        .map_err(|error| LspError::invalid_params(format!("invalid URI: {error}")))?;
    let document =
        ctx.workspace().text_document(&uri).await.map_err(|error| {
            LspError::invalid_request(format!("cannot read `{uri:?}`: {error}"))
        })?;
    Ok(document.text())
}

fn completion_options() -> CompletionOptions {
    CompletionOptions {
        trigger_characters: Some(vec![".".to_string()]),
        ..CompletionOptions::default()
    }
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    // Logs go to stderr: stdout carries the LSP wire protocol and nothing else.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let server = Server::builder(State::new())
        // Unopened `file:` URIs resolve from the local filesystem.
        .file_provider(OsFileProvider::new())
        // Standard features: the registrations are the capability catalog.
        .feature(lspf::features::hover(), hover)
        .feature(lspf::features::completion(completion_options()), completion)
        .feature(lspf::features::completion_resolve(), resolve_completion)
        // Typed Commands beneath `workspace/executeCommand`.
        .command("lspf-hello.workspaceRoots", workspace_roots)
        .command("lspf-hello.readFile", read_file)
        // The post-mutation hook observes the framework's document sync.
        .notification::<DidOpenTextDocument, _, _>(on_did_open)
        .build()
        .expect("the static registrations are valid");
    // Serving reports how the connection ended and never terminates the
    // process; turning that Outcome into a process disposition is this
    // binary's decision.
    let outcome = lspf::stdio(server).serve().await?;
    std::process::exit(outcome.code());
}
