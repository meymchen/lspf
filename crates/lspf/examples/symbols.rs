//! Document and Workspace Symbols server for a small `type`/`fn` language.

mod example_support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lspf::types::notification::{DidChangeTextDocument, DidOpenTextDocument};
use lspf::types::{
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, DocumentSymbol, DocumentSymbolOptions,
    DocumentSymbolParams, DocumentSymbolResponse, Location, Position, Range, SymbolInformation,
    SymbolKind, Uri, WorkspaceSymbolOptions, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use lspf::{CancellationToken, LspError, Server, ServerContext};

#[derive(Clone)]
struct IndexedSymbol {
    name: String,
    kind: SymbolKind,
    range: Range,
}

#[derive(Default)]
struct State {
    index: Mutex<HashMap<Uri, Vec<IndexedSymbol>>>,
}

fn parse(text: &str) -> Vec<IndexedSymbol> {
    text.lines()
        .enumerate()
        .filter_map(|(line_number, line)| {
            let (prefix, kind) = if line.starts_with("type ") {
                ("type ", SymbolKind::CLASS)
            } else if line.starts_with("fn ") {
                ("fn ", SymbolKind::FUNCTION)
            } else {
                return None;
            };
            let name = line[prefix.len()..]
                .split(['(', ' ', '='])
                .next()?
                .to_string();
            let range = Range::new(
                Position::new(line_number as u32, prefix.len() as u32),
                Position::new(line_number as u32, (prefix.len() + name.len()) as u32),
            );
            Some(IndexedSymbol { name, kind, range })
        })
        .collect()
}

fn update(state: &State, ctx: &ServerContext, uri: &Uri) {
    let Some(document) = ctx.documents().get(uri) else {
        return;
    };
    state
        .index
        .lock()
        .unwrap()
        .insert(uri.clone(), parse(&document.text()));
}

async fn did_open(state: Arc<State>, ctx: ServerContext, params: DidOpenTextDocumentParams) {
    update(&state, &ctx, &params.text_document.uri);
}

async fn did_change(state: Arc<State>, ctx: ServerContext, params: DidChangeTextDocumentParams) {
    update(&state, &ctx, &params.text_document.uri);
}

#[allow(deprecated)]
async fn document_symbols(
    state: Arc<State>,
    ctx: ServerContext,
    params: DocumentSymbolParams,
    _: CancellationToken,
) -> Result<Option<DocumentSymbolResponse>, LspError> {
    update(&state, &ctx, &params.text_document.uri);
    let index = state.index.lock().unwrap();
    let symbols = index
        .get(&params.text_document.uri)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|symbol| DocumentSymbol {
            name: symbol.name,
            detail: None,
            kind: symbol.kind,
            tags: None,
            deprecated: None,
            range: symbol.range,
            selection_range: symbol.range,
            children: None,
        })
        .collect();
    Ok(Some(DocumentSymbolResponse::Nested(symbols)))
}

#[allow(deprecated)]
async fn workspace_symbols(
    state: Arc<State>,
    _: ServerContext,
    params: WorkspaceSymbolParams,
    _: CancellationToken,
) -> Result<Option<WorkspaceSymbolResponse>, LspError> {
    let query = params.query.to_lowercase();
    let index = state.index.lock().unwrap();
    let symbols = index
        .iter()
        .flat_map(|(uri, symbols)| {
            symbols
                .iter()
                .filter(|symbol| symbol.name.to_lowercase().contains(&query))
                .map(|symbol| SymbolInformation {
                    name: symbol.name.clone(),
                    kind: symbol.kind,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range: symbol.range,
                    },
                    container_name: None,
                })
        })
        .collect();
    Ok(Some(WorkspaceSymbolResponse::Flat(symbols)))
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State::default())
        .notification::<DidOpenTextDocument, _, _>(did_open)
        .notification::<DidChangeTextDocument, _, _>(did_change)
        .feature(
            lspf::features::document_symbol(DocumentSymbolOptions {
                label: Some("lspf symbols example".to_string()),
                work_done_progress_options: Default::default(),
            }),
            document_symbols,
        )
        .feature(
            lspf::features::workspace_symbol(WorkspaceSymbolOptions {
                work_done_progress_options: Default::default(),
                resolve_provider: None,
            }),
            workspace_symbols,
        )
        .build()
        .expect("symbol registrations are valid");
    example_support::serve(server).await
}
