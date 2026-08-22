//! Document Links server.
//! It recognizes `<github:org/repo>` and `<pypi:project>` and resolves lazily.

mod example_support;

use std::sync::Arc;

use lspf::types::{DocumentLink, DocumentLinkOptions, DocumentLinkParams, Position, Range};
use lspf::{CancellationToken, Context, LspError, Server};
use serde_json::{Value, json};

struct State;

async fn links(
    _: Arc<State>,
    ctx: Context,
    params: DocumentLinkParams,
    _: CancellationToken,
) -> Result<Option<Vec<DocumentLink>>, LspError> {
    let text = example_support::text(&ctx, &params.text_document.uri)?;
    let mut links = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        let mut offset = 0;
        while let Some(start_relative) = line[offset..].find('<') {
            let start = offset + start_relative;
            let Some(end_relative) = line[start + 1..].find('>') else {
                break;
            };
            let end = start + end_relative + 2;
            if let Some((kind, target)) = line[start + 1..end - 1].split_once(':')
                && !kind.is_empty()
                && !target.is_empty()
            {
                links.push(DocumentLink {
                    range: Range::new(
                        Position::new(line_number as u32, start as u32),
                        Position::new(line_number as u32, end as u32),
                    ),
                    target: None,
                    tooltip: None,
                    data: Some(json!({ "type": kind, "target": target })),
                });
            }
            offset = end;
        }
    }
    Ok(Some(links))
}

async fn resolve(
    _: Arc<State>,
    _: Context,
    mut link: DocumentLink,
    _: CancellationToken,
) -> Result<DocumentLink, LspError> {
    let data = link.data.as_ref().and_then(Value::as_object);
    let kind = data
        .and_then(|data| data.get("type"))
        .and_then(Value::as_str);
    let target = data
        .and_then(|data| data.get("target"))
        .and_then(Value::as_str);
    if let (Some(kind), Some(target)) = (kind, target) {
        let (url, tooltip) = match kind {
            "github" => (
                format!("https://github.com/{target}"),
                format!("GitHub - {target}"),
            ),
            "pypi" => (
                format!("https://pypi.org/project/{target}"),
                format!("PyPI - {target}"),
            ),
            _ => return Ok(link),
        };
        link.target = Some(
            url.parse()
                .map_err(|_| LspError::invalid_params("invalid link"))?,
        );
        link.tooltip = Some(tooltip);
    }
    Ok(link)
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .feature(
            lspf::features::document_link(DocumentLinkOptions {
                resolve_provider: None,
                work_done_progress_options: Default::default(),
            }),
            links,
        )
        .feature(lspf::features::document_link_resolve(), resolve)
        .build()
        .expect("document-link registrations are valid");
    example_support::serve(server).await
}
