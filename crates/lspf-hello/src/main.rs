//! The template language server: a built `Server` served over stdio.
//!
//! It registers one post-mutation hook for `textDocument/didOpen`, reads the
//! document the framework has already opened, and publishes an informational
//! diagnostic through the typed `Client` handle. Fork it as the starting point
//! for a real server.

use std::sync::Arc;

use lspf::types::notification::{DidOpenTextDocument, PublishDiagnostics};
use lspf::types::{
    Diagnostic, DiagnosticSeverity, DidOpenTextDocumentParams, Position, PublishDiagnosticsParams,
    Range,
};
use lspf::{Context, Server};
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

#[tokio::main]
async fn main() -> lspf::Result<()> {
    // Logs go to stderr: stdout carries the LSP wire protocol and nothing else.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let server = Server::builder(State::new())
        .notification::<DidOpenTextDocument, _, _>(on_did_open)
        .build()
        .expect("the static registrations are valid");
    let outcome = lspf::stdio(server).serve().await?;
    // Serving reports how the connection ended and never terminates the
    // process; turning that Outcome into a process disposition is this
    // binary's decision.
    std::process::exit(outcome.code());
}
