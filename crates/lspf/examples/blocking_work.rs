//! Blocking-work server.
//!
//! lspf handlers are async-only. Blocking work is moved to `spawn_blocking`;
//! the async command awaits it while other handlers continue under the
//! server's bounded concurrent dispatcher.

mod example_support;

use std::sync::Arc;
use std::time::Duration;

use lspf::types::{
    CompletionItem, CompletionOptions, CompletionParams, CompletionResponse, MessageType,
    ShowMessageParams,
};
use lspf::{CancellationToken, LspError, Server, ServerContext};

struct State;

async fn completions(
    _: Arc<State>,
    _: ServerContext,
    _: CompletionParams,
    _: CancellationToken,
) -> Result<Option<CompletionResponse>, LspError> {
    Ok(Some(CompletionResponse::Array(
        ["one", "two", "three", "four", "five"]
            .into_iter()
            .map(|label| CompletionItem {
                label: label.to_string(),
                ..CompletionItem::default()
            })
            .collect(),
    )))
}

async fn count_down(
    _: Arc<State>,
    ctx: ServerContext,
    _: (),
    _: CancellationToken,
) -> Result<(), LspError> {
    let client = ctx.client();
    tokio::task::spawn_blocking(move || {
        for value in (0..10).rev() {
            std::thread::sleep(Duration::from_millis(100));
            client
                .show_message(ShowMessageParams {
                    typ: MessageType::INFO,
                    message: value.to_string(),
                })
                .map_err(LspError::internal)?;
        }
        Ok(())
    })
    .await
    .map_err(|error| LspError::internal(error.to_string()))?
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .feature(
            lspf::features::completion(CompletionOptions {
                trigger_characters: Some(vec![".".to_string()]),
                ..CompletionOptions::default()
            }),
            completions,
        )
        .command::<(), (), _, _>("count.down.blocking", count_down)
        .build()
        .expect("concurrent-handler registrations are valid");
    example_support::serve(server).await
}
