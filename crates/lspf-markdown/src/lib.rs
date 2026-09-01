//! First-party Markdown link language server.

mod parser;
mod target;

use std::sync::Arc;

use lspf::types::{
    Contents, Definition, DefinitionOptions, DefinitionParams, DefinitionResponse, Diagnostic,
    DiagnosticSeverity, DidChangeTextDocumentNotification as DidChangeTextDocument,
    DidChangeTextDocumentParams, DidOpenTextDocumentNotification as DidOpenTextDocument,
    DidOpenTextDocumentParams, Hover, HoverParams, Location, MarkupContent, MarkupKind, Position,
    PublishDiagnosticsNotification as PublishDiagnostics, PublishDiagnosticsParams, Range, Uri,
};
use lspf::{CancellationToken, FileProvider, LspError, Server, ServerContext};

use parser::markdown_links;
use target::{Heading, LocalTarget, resolve_local_target, selected_heading};

const SOURCE: &str = "lspf-markdown";

/// Application state for the Markdown server.
pub struct State;

#[derive(Debug)]
struct TargetAtPosition {
    source_range: Range,
    local: LocalTarget,
    heading: Option<Heading>,
}

async fn target_at_position(
    ctx: &ServerContext,
    uri: &Uri,
    position: Position,
) -> Option<TargetAtPosition> {
    let document = ctx.documents().get(uri)?;
    let encoding = ctx.documents().position_encoding();
    let offset = document.position_to_offset(encoding, position)?;
    let link = markdown_links(&document.text())
        .into_iter()
        .find(|link| link.target_start <= offset && offset <= link.target_end)?;
    let local = resolve_local_target(uri, &link.target)?;
    let target = ctx.workspace().text_document(&local.uri).await.ok()?;
    let heading = selected_heading(&target, encoding, local.fragment.as_deref());
    let source_range = Range::new(
        document.offset_to_position(encoding, link.target_start)?,
        document.offset_to_position(encoding, link.target_end)?,
    );
    Some(TargetAtPosition {
        source_range,
        local,
        heading,
    })
}

async fn publish_diagnostics(ctx: ServerContext, uri: Uri) {
    let Some(document) = ctx.documents().get(&uri) else {
        return;
    };
    let text = document.text();
    let mut diagnostics = Vec::new();
    for link in markdown_links(&text) {
        let Some(local) = resolve_local_target(&uri, &link.target) else {
            continue;
        };
        let target = ctx.workspace().text_document(&local.uri).await;
        let missing_heading = target.as_ref().is_ok_and(|target| {
            local.fragment.is_some()
                && selected_heading(
                    target,
                    ctx.documents().position_encoding(),
                    local.fragment.as_deref(),
                )
                .is_none()
        });
        if target.is_ok() && !missing_heading {
            continue;
        }
        let Some(start) = ctx.documents().offset_to_position(&uri, link.target_start) else {
            continue;
        };
        let Some(end) = ctx.documents().offset_to_position(&uri, link.target_end) else {
            continue;
        };
        diagnostics.push(Diagnostic {
            range: Range::new(start, end),
            severity: Some(DiagnosticSeverity::Error),
            source: Some(SOURCE.into()),
            message: if missing_heading {
                format!("local link heading does not exist: {}", link.target).into()
            } else {
                format!("local link target does not exist: {}", link.target).into()
            },
            ..Diagnostic::default()
        });
    }

    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: document.version(),
    };
    let _ = ctx.client().notify::<PublishDiagnostics>(params);
}

async fn did_open(_state: Arc<State>, ctx: ServerContext, params: DidOpenTextDocumentParams) {
    publish_diagnostics(ctx, params.text_document.uri).await;
}

async fn did_change(_state: Arc<State>, ctx: ServerContext, params: DidChangeTextDocumentParams) {
    publish_diagnostics(ctx, params.text_document.text_document_identifier.uri).await;
}

async fn hover(
    _state: Arc<State>,
    ctx: ServerContext,
    params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let Some(target) =
        target_at_position(&ctx, &uri, params.text_document_position_params.position).await
    else {
        return Ok(None);
    };
    let heading = target
        .heading
        .as_ref()
        .map_or_else(|| target.local.display(), |heading| heading.title.clone());

    Ok(Some(Hover {
        contents: Contents::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!(
                "**{heading}**\n\n{tick}{}{tick}",
                target.local.display(),
                tick = char::from(96)
            ),
        }),
        range: Some(target.source_range),
    }))
}

async fn definition(
    _state: Arc<State>,
    ctx: ServerContext,
    params: DefinitionParams,
    _ct: CancellationToken,
) -> Result<Option<DefinitionResponse>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let Some(target) =
        target_at_position(&ctx, &uri, params.text_document_position_params.position).await
    else {
        return Ok(None);
    };

    Ok(Some(DefinitionResponse::Definition(Definition::Location(
        Location {
            uri: target.local.uri,
            range: target.heading.map_or_else(
                || Range::new(Position::new(0, 0), Position::new(0, 0)),
                |heading| heading.range,
            ),
        },
    ))))
}

/// Build a Markdown server with the caller's file provider.
pub fn server(file_provider: impl FileProvider) -> Server<State> {
    Server::builder(State)
        .file_provider(file_provider)
        .feature(lspf::features::hover(), hover)
        .feature(
            lspf::features::definition(DefinitionOptions {
                work_done_progress_options: Default::default(),
            }),
            definition,
        )
        .notification::<DidOpenTextDocument, _, _>(did_open)
        .notification::<DidChangeTextDocument, _, _>(did_change)
        .build()
        .expect("valid registrations")
}
