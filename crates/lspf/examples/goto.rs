//! Goto "X" and Find References server.
//! The toy language declares types with `type Name(` and functions with `fn name(`.

mod example_support;

use std::sync::Arc;

use lspf::types::request::{
    GotoDeclarationParams, GotoDeclarationResponse, GotoImplementationParams,
    GotoImplementationResponse, GotoTypeDefinitionParams, GotoTypeDefinitionResponse,
};
use lspf::types::{
    DeclarationOptions, DefinitionOptions, GotoDefinitionParams, GotoDefinitionResponse, Location,
    Position, ReferenceParams, ReferencesOptions, StaticTextDocumentRegistrationOptions,
};
use lspf::{CancellationToken, LspError, Server, ServerContext};

struct State;

fn definition_location(
    text: &str,
    uri: &lspf::types::Uri,
    name: &str,
    prefix: &str,
) -> Option<Location> {
    text.lines().enumerate().find_map(|(line_number, line)| {
        let rest = line.strip_prefix(prefix)?;
        let candidate = rest.split(['(', ' ', '=']).next()?;
        (candidate == name).then(|| Location {
            uri: uri.clone(),
            range: lspf::types::Range::new(
                Position::new(line_number as u32, prefix.len() as u32),
                Position::new(line_number as u32, (prefix.len() + name.len()) as u32),
            ),
        })
    })
}

fn selected(
    ctx: &ServerContext,
    uri: &lspf::types::Uri,
    position: Position,
) -> Result<(String, String), LspError> {
    let text = example_support::text(ctx, uri)?;
    let word = example_support::word_at(&text, position)
        .map(|(word, _)| word)
        .ok_or_else(|| LspError::invalid_params("no symbol at position"))?;
    Ok((text, word))
}

async fn declaration(
    _: Arc<State>,
    ctx: ServerContext,
    params: GotoDeclarationParams,
    _: CancellationToken,
) -> Result<Option<GotoDeclarationResponse>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let (text, word) = selected(&ctx, &uri, params.text_document_position_params.position)?;
    Ok(definition_location(&text, &uri, &word, "fn ").map(GotoDeclarationResponse::Scalar))
}

async fn definition(
    _: Arc<State>,
    ctx: ServerContext,
    params: GotoDefinitionParams,
    _: CancellationToken,
) -> Result<Option<GotoDefinitionResponse>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let (text, word) = selected(&ctx, &uri, params.text_document_position_params.position)?;
    Ok(definition_location(&text, &uri, &word, "type ").map(GotoDefinitionResponse::Scalar))
}

async fn implementation(
    _: Arc<State>,
    ctx: ServerContext,
    params: GotoImplementationParams,
    _: CancellationToken,
) -> Result<Option<GotoImplementationResponse>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let (text, word) = selected(&ctx, &uri, params.text_document_position_params.position)?;
    Ok(definition_location(&text, &uri, &word, "fn ").map(GotoImplementationResponse::Scalar))
}

async fn type_definition(
    _: Arc<State>,
    ctx: ServerContext,
    params: GotoTypeDefinitionParams,
    _: CancellationToken,
) -> Result<Option<GotoTypeDefinitionResponse>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let (text, word) = selected(&ctx, &uri, position)?;
    let line = text.lines().nth(position.line as usize).unwrap_or_default();
    let type_name = line.split(',').find_map(|argument| {
        let (name, kind) = argument.split_once(':')?;
        (name.trim().trim_start_matches("fn ") == word)
            .then(|| kind.split_whitespace().next().unwrap_or(kind.trim()))
    });
    Ok(type_name
        .and_then(|name| definition_location(&text, &uri, name, "type "))
        .map(GotoTypeDefinitionResponse::Scalar))
}

async fn references(
    _: Arc<State>,
    ctx: ServerContext,
    params: ReferenceParams,
    _: CancellationToken,
) -> Result<Option<Vec<Location>>, LspError> {
    let uri = params.text_document_position.text_document.uri;
    let (text, word) = selected(&ctx, &uri, params.text_document_position.position)?;
    Ok(Some(
        example_support::word_ranges(&text, &word)
            .into_iter()
            .map(|range| Location {
                uri: uri.clone(),
                range,
            })
            .collect(),
    ))
}

fn static_options() -> StaticTextDocumentRegistrationOptions {
    StaticTextDocumentRegistrationOptions {
        document_selector: None,
        id: None,
    }
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .feature(
            lspf::features::declaration(DeclarationOptions {
                work_done_progress_options: Default::default(),
            }),
            declaration,
        )
        .feature(
            lspf::features::definition(DefinitionOptions {
                work_done_progress_options: Default::default(),
            }),
            definition,
        )
        .feature(
            lspf::features::implementation(static_options()),
            implementation,
        )
        .feature(
            lspf::features::type_definition(static_options()),
            type_definition,
        )
        .feature(
            lspf::features::references(ReferencesOptions {
                work_done_progress_options: Default::default(),
            }),
            references,
        )
        .build()
        .expect("navigation registrations are valid");
    example_support::serve(server).await
}
