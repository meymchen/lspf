//! Server features that span several LSP methods.
//!
//! This server demonstrates typed commands, progress, client configuration,
//! async handlers, and dynamic client registration. TCP and WebSocket hosting
//! are covered by the adjacent `native_tcp` and `native_websocket` examples.

mod example_support;

use std::sync::Arc;

use lspf::types::request::Completion;
use lspf::types::{
    CompletionItem, CompletionParams, CompletionResponse, ConfigurationItem, ConfigurationParams,
    Registration, RegistrationParams, Unregistration, UnregistrationParams,
};
use lspf::{CancellationToken, LspError, ProgressOptions, Server, ServerContext};
use serde_json::{Value, json};

struct State;

async fn completions(
    _: Arc<State>,
    _: ServerContext,
    _: CompletionParams,
    _: CancellationToken,
) -> Result<Option<CompletionResponse>, LspError> {
    tokio::task::yield_now().await;
    Ok(Some(CompletionResponse::CompletionItemList(
        ["null", "true", "false"]
            .into_iter()
            .map(|label| CompletionItem {
                label: label.to_string(),
                ..CompletionItem::default()
            })
            .collect(),
    )))
}

async fn progress(
    _: Arc<State>,
    ctx: ServerContext,
    _: (),
    _: CancellationToken,
) -> Result<(), LspError> {
    let handle = ctx
        .client()
        .begin_progress(ProgressOptions::new("JSON server progress").percentage(0))
        .await
        .map_err(LspError::internal)?;
    for percentage in [25, 50, 75, 100] {
        tokio::task::yield_now().await;
        handle
            .report(Some(format!("{percentage}%")), Some(percentage))
            .map_err(LspError::internal)?;
    }
    handle
        .end(Some("done".to_string()))
        .map_err(LspError::internal)
}

async fn configuration(
    _: Arc<State>,
    ctx: ServerContext,
    _: (),
    _: CancellationToken,
) -> Result<Value, LspError> {
    let values = ctx
        .client()
        .configuration(ConfigurationParams {
            items: vec![ConfigurationItem {
                scope_uri: None,
                section: Some("lspf.jsonServer".to_string()),
            }],
        })
        .await
        .map_err(LspError::internal)?;
    Ok(values.into_iter().next().unwrap_or(Value::Null))
}

async fn register_completions(
    _: Arc<State>,
    ctx: ServerContext,
    _: (),
    _: CancellationToken,
) -> Result<(), LspError> {
    ctx.client()
        .register_capability(RegistrationParams {
            registrations: vec![Registration {
                id: "json-completions".to_string(),
                method: "textDocument/completion".to_string(),
                register_options: Some(json!({ "triggerCharacters": ["\""] })),
            }],
        })
        .await
        .map_err(LspError::internal)
}

async fn unregister_completions(
    _: Arc<State>,
    ctx: ServerContext,
    _: (),
    _: CancellationToken,
) -> Result<(), LspError> {
    ctx.client()
        .unregister_capability(UnregistrationParams {
            unregisterations: vec![Unregistration {
                id: "json-completions".to_string(),
                method: "textDocument/completion".to_string(),
            }],
        })
        .await
        .map_err(LspError::internal)
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        // A typed standard request route without a static capability lets the
        // commands below advertise and withdraw it dynamically at the client.
        .request::<Completion, _, _>(completions)
        .command::<(), (), _, _>("progress", progress)
        .command::<(), Value, _, _>("showConfigurationAsync", configuration)
        .command::<(), (), _, _>("registerCompletions", register_completions)
        .command::<(), (), _, _>("unregisterCompletions", unregister_completions)
        .build()
        .expect("JSON-server registrations are valid");
    example_support::serve(server).await
}
