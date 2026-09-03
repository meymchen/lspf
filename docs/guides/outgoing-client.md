# Outgoing client helpers

This guide covers the server-to-client half of a connection: the typed
[`ClientHandle`] a handler reaches through [`ServerContext`], the named helpers
it offers for every standard outgoing notification and request, dynamic
capability registration, work-done progress, partial-result reporting, and the
escape hatches for custom methods. Nothing here is handwritten JSON — every
helper takes native [`lspf::types`] parameters and returns native results, and
every wire shape is pinned by a fixture under
`crates/lspf/tests/fixtures`. Every example here
compiles as a doctest against the shipped crate, and the complete journey —
configuration lookup, workspace edit, a custom outgoing request, dynamic
registration, every stable refresh, and cancellable progress — runs as a real
server in [`crates/lspf-hello`](../../crates/lspf-hello/src/main.rs) with an
end-to-end stdio test beside it.

## Notifications

A notification is fire-and-forget: it is encoded and enqueued synchronously,
allocates no ID, and returns `ClientError::OutboundOverloaded` if the
connection's message-count or encoded-byte budget is full. It can also fail if
the connection is closing or the params cannot be encoded. The named helpers
cover the stable outgoing surface —
[`publish_diagnostics`](lspf::ClientHandle::publish_diagnostics),
[`show_message`](lspf::ClientHandle::show_message),
[`log_message`](lspf::ClientHandle::log_message),
[`log_trace`](lspf::ClientHandle::log_trace),
[`telemetry_event`](lspf::ClientHandle::telemetry_event), and
[`progress`](lspf::ClientHandle::progress) — each sending its params exactly as
provided:

```rust
# use lspf::types::{MessageType, ShowMessageParams};
# use lspf::{ClientHandle, ClientError};
# fn announce(client: &ClientHandle) {
// The message goes to the client only; it is never echoed into the server's
// local tracing stream.
if let Err(error) = client.show_message(ShowMessageParams {
    kind: MessageType::Info,
    message: "reindexing finished".into(),
}) {
    tracing::warn!(%error, "show_message failed");
}
# }
```

Two helpers carry extra contract worth knowing. `log_trace` gates on the
connection's trace level: while the level is `Off` — the initial value until
the client sends `$/setTrace` — it enqueues nothing and returns `Ok(())`, and
sending never changes the level. `publish_diagnostics` sends the params
verbatim, caller-provided `version` included; the framework neither caches
nor deduplicates diagnostics, and closing a document never clears them
automatically. [`ServerContext::publish_diagnostics`](lspf::ServerContext::publish_diagnostics)
is a convenience forward to the client helper and returns the same
`Result<(), ClientError>`.

## Window and workspace requests

A request reserves a connection-local, never-reused ID and asynchronously
awaits its correlated response. The named helpers cover the standard window
interactions — [`show_document`](lspf::ClientHandle::show_document),
[`show_message_request`](lspf::ClientHandle::show_message_request) — and the
workspace interactions — [`apply_edit`](lspf::ClientHandle::apply_edit),
[`configuration`](lspf::ClientHandle::configuration),
[`workspace_folders`](lspf::ClientHandle::workspace_folders). All of them are thin
wrappers over the same typed request broker: params go out verbatim, results
come back verbatim, and no helper adds UI, message-selection, or edit policy.

A configuration lookup returns one JSON value per requested item, in order,
with `null` for unanswered items. The result goes to the caller only — it
never writes into the framework-owned [`Workspace`](lspf::Workspace) snapshot,
which stays under `workspace/didChangeConfiguration` notification sync:

```rust
# use lspf::types::{ConfigurationItem, ConfigurationParams};
# use lspf::{ClientHandle, ClientError};
async fn tab_size(client: &ClientHandle) -> Result<Option<u64>, ClientError> {
    let values = client
        .configuration(ConfigurationParams {
            items: vec![ConfigurationItem {
                scope_uri: None,
                section: Some("editor.tabSize".into()),
            }],
        })
        .await?;
    Ok(values.first().and_then(|value| value.as_u64()))
}
```

An edit is sent exactly as built — never filtered, batched, or rewritten —
and the client's verdict comes back verbatim:

```rust
# use lspf::types::{ApplyWorkspaceEditParams, Position, Range, TextEdit, Uri, WorkspaceEdit};
# use lspf::{ClientHandle, ClientError};
async fn insert_header(client: &ClientHandle, uri: Uri) -> Result<bool, ClientError> {
    let edit = WorkspaceEdit {
        changes: Some(
            [(
                uri,
                vec![TextEdit {
                    range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                    new_text: "// generated\n".into(),
                }],
            )]
            .into_iter()
            .collect(),
        ),
        ..WorkspaceEdit::default()
    };
    let response = client
        .apply_edit(ApplyWorkspaceEditParams {
            label: Some("insert header".into()),
            edit,
            metadata: None,
        })
        .await?;
    Ok(response.applied)
}
```

## Dynamic registration

[`register_capability`](lspf::ClientHandle::register_capability) and
[`unregister_capability`](lspf::ClientHandle::unregister_capability) announce
capability changes to the client at runtime. They are pure announcements:
the permanently frozen [`Router`](lspf::Server) and the computed initialize
capabilities stay untouched, the framework retains no second list of
currently registered client capabilities, and any local route the
registration points at must already exist through static or
initialize-conditional registration.

```rust
# use lspf::types::{Registration, RegistrationParams};
# use lspf::{ClientHandle, ClientError};
async fn watch_files(client: &ClientHandle) -> Result<(), ClientError> {
    client
        .register_capability(RegistrationParams {
            registrations: vec![Registration {
                id: "my-server.watch".into(),
                method: "workspace/didChangeWatchedFiles".into(),
                register_options: None,
            }],
        })
        .await
}
```

## Workspace refresh

Seven helpers ask the client to re-pull a stable workspace feature —
[`refresh_code_lenses`](lspf::ClientHandle::refresh_code_lenses),
[`refresh_diagnostics`](lspf::ClientHandle::refresh_diagnostics),
[`refresh_inlay_hints`](lspf::ClientHandle::refresh_inlay_hints),
[`refresh_inline_values`](lspf::ClientHandle::refresh_inline_values),
[`refresh_semantic_tokens`](lspf::ClientHandle::refresh_semantic_tokens),
[`refresh_folding_ranges`](lspf::ClientHandle::refresh_folding_ranges), and
[`refresh_text_document_content`](lspf::ClientHandle::refresh_text_document_content).
All except `refresh_text_document_content` take no parameters; that helper
names the target document URI. Each returns the client's `null`
acknowledgement as `()`. The helpers trigger no recomputation, and the
framework stores no feature state for them.

The last two are the LSP 3.18 additions. Both are stable now; they were briefly
gated behind a `proposed` Cargo feature, which has since been removed along
with the `lspf::proposed` aliases.

## Work-done progress

[`begin_progress`](lspf::ClientHandle::begin_progress) runs the connection-scoped
work-done lifecycle as one failure-safe operation: it allocates a
connection-local numeric token from a monotonic sequence, completes
`window/workDoneProgress/create`, registers the token only after the remote
success, and enqueues exactly one begin notification carrying the options
verbatim. The returned [`ProgressHandle`] reports and ends; `end` consumes
the handle and removes the token whether the enqueue succeeded or failed, so
a lifecycle never leaks its token.

```rust
# use lspf::{ClientHandle, ClientError, ProgressOptions};
async fn index_workspace(client: &ClientHandle) -> Result<(), ClientError> {
    let progress = client
        .begin_progress(
            ProgressOptions::new("Indexing")
                .cancellable(true)
                .message("starting")
                .percentage(0),
        )
        .await?;
    for done in 1..=4u32 {
        // The client cancels through `window/workDoneProgress/cancel`; the
        // framework fires this token and sends nothing by itself. What
        // happens next — including the final message — is the application's
        // choice.
        if progress.cancellation_token().is_cancelled() {
            break;
        }
        progress.report(None, Some(done * 25))?;
    }
    progress.end(Some("index ready".into()))?;
    Ok(())
}
```

A failed begin leaves no registered token behind. Report percentages outside
0 through 100 fail before anything is sent. Dropping an active handle removes
its token with a warning but performs no I/O — there is no implicit end, so
always call `end` deliberately.

## Partial results

Work-done progress reports *how far along* a request is; a partial result
sends *the answer itself*, chunk by chunk, so the editor can render early
matches while the handler is still working. Both travel as `$/progress`, but
the partial-result path is typed to the request being handled rather than to a
progress lifecycle, so it is a separate surface (ADR 0033). The sink admits
discrete protocol messages; it is not an async iterator, and abandoning it
needs no cleanup.

[`ServerContext::partial_results`](lspf::ServerContext::partial_results) lends
the handler a [`PartialResultSink`] for the request it is currently serving. It
returns `Some` only when all three hold:

- the marker `R` is the method being handled — asking for another method's sink
  yields `None`, never a mis-routed chunk;
- the LSP metaModel defines a partial result for that method, which is what the
  sealed [`PartialResultRequest`] trait encodes. Notifications, custom
  requests, and standard methods without a partial result cannot implement it;
- the client supplied a `partialResultToken` on this request.

So a handler always needs the single-response path too — a client that sends no
token still expects a complete result:

```rust
# use std::sync::Arc;
use lspf::types::request::DocumentSymbolRequest;
use lspf::types::{
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolPartialResponse, DocumentSymbolResponse,
};
# use lspf::{CancellationToken, ServerContext, LspError};
# struct State;
async fn document_symbols(
    _state: Arc<State>,
    ctx: ServerContext,
    _params: DocumentSymbolParams,
    _ct: CancellationToken,
) -> Result<Option<DocumentSymbolResponse>, LspError> {
    let symbols: Vec<DocumentSymbol> = find_symbols();
    let Some(sink) = ctx.partial_results::<DocumentSymbolRequest>() else {
        // No token: answer the ordinary way, in one response.
        return Ok(Some(DocumentSymbolResponse::DocumentSymbolList(symbols)));
    };
    for symbol in symbols {
        sink.report(DocumentSymbolPartialResponse::DocumentSymbolList(vec![symbol]))
            .map_err(LspError::internal)?;
    }
    // Every chunk already went out; the response completes the request.
    Ok(None)
}
# fn find_symbols() -> Vec<DocumentSymbol> { Vec::new() }
```

`report` is synchronous for the same reason every notification helper is: it
admits one `$/progress` message to the connection's bounded outbound queue,
under the same message-count and exact-byte budgets, and a full queue returns
`ClientError::OutboundOverloaded` while retaining no part of the rejected
chunk. Chunks enter the same FIFO as the response, so they keep call order and
always precede it.

There is no finish message and no `end` to call: the request's normal response
is what concludes the reporting, and dropping the sink performs no I/O. The
sink is gated on
the request's lifetime rather than on your holding it — a report attempted
after the handler has completed, including one from a cloned `ServerContext`,
fails with `ClientError::InvalidHelperParams` instead of racing past the
response.

## Custom requests and notifications

Non-standard outgoing methods need no new machinery: define a marker type
implementing [`lsp_types::request::Request`](lspf::types::request::Request)
or [`lsp_types::notification::Notification`](lspf::types::notification::Notification),
then call the generic [`request`](lspf::ClientHandle::request) or
[`notify`](lspf::ClientHandle::notify):

```rust
use lspf::types::request::Request;
use lspf::{ClientHandle, ClientError};

enum SyntaxTree {}

impl Request for SyntaxTree {
    type Params = serde_json::Value;
    type Result = String;
    const METHOD: &'static str = "rust-analyzer/syntaxTree";
}

async fn syntax_tree(client: &ClientHandle, params: serde_json::Value) -> Result<String, ClientError> {
    client.request::<SyntaxTree>(params).await
}
```

The broker's semantics apply uniformly: responses may arrive in any order and
are correlated by ID; abandoning the future before the response arrives
removes the pending entry and emits one `$/cancelRequest`; session close
resolves every pending request with `ClientError::Cancelled`; a peer's
JSON-RPC error surfaces as [`ClientError::Remote`](lspf::ClientError::Remote)
carrying the full code, message, and data. A request rejected by the outbound
message or byte budget returns `ClientError::OutboundOverloaded` and removes
the pending entry before returning. By default, a request that remains pending
for 30 seconds returns [`ClientError::Timeout`](lspf::ClientError::Timeout) and
attempts one `$/cancelRequest`. Configure the duration through
`ResourcePolicy::outbound_request_timeout`, or set it to `None` to wait without
a deadline. Expired IDs are never reused, so a late response cannot complete a
later request.

## Helper reference

Every named helper, its wire method, its parameter and result types, and its
stability status. Notification helpers and `ProgressHandle::report`/`end`
return `Result<(), ClientError>`; request helpers return
`Result<R, ClientError>` with the `R` shown below.

| Rust method | Wire method | Parameters | Result | Status |
| --- | --- | --- | --- | --- |
| `ClientHandle::publish_diagnostics` | `textDocument/publishDiagnostics` | `PublishDiagnosticsParams` | `()` | stable |
| `ClientHandle::show_message` | `window/showMessage` | `ShowMessageParams` | `()` | stable |
| `ClientHandle::log_message` | `window/logMessage` | `LogMessageParams` | `()` | stable |
| `ClientHandle::log_trace` | `$/logTrace` | `LogTraceParams` | `()` | stable |
| `ClientHandle::telemetry_event` | `telemetry/event` | `TelemetryEventParams` | `()` | stable |
| `ClientHandle::progress` | `$/progress` | `ProgressParams` | `()` | stable |
| `ServerContext::publish_diagnostics` | `textDocument/publishDiagnostics` | `PublishDiagnosticsParams` | `()` | stable |
| `ClientHandle::show_document` | `window/showDocument` | `ShowDocumentParams` | `ShowDocumentResult` | stable |
| `ClientHandle::show_message_request` | `window/showMessageRequest` | `ShowMessageRequestParams` | `Option<MessageActionItem>` | stable |
| `ClientHandle::apply_edit` | `workspace/applyEdit` | `ApplyWorkspaceEditParams` | `ApplyWorkspaceEditResult` | stable |
| `ClientHandle::configuration` | `workspace/configuration` | `ConfigurationParams` | `Vec<serde_json::Value>` | stable |
| `ClientHandle::workspace_folders` | `workspace/workspaceFolders` | none (`null`) | `Option<Vec<WorkspaceFolder>>` | stable |
| `ClientHandle::register_capability` | `client/registerCapability` | `RegistrationParams` | `()` | stable |
| `ClientHandle::unregister_capability` | `client/unregisterCapability` | `UnregistrationParams` | `()` | stable |
| `ClientHandle::refresh_code_lenses` | `workspace/codeLens/refresh` | none (`null`) | `()` | stable |
| `ClientHandle::refresh_diagnostics` | `workspace/diagnostic/refresh` | none (`null`) | `()` | stable |
| `ClientHandle::refresh_inlay_hints` | `workspace/inlayHint/refresh` | none (`null`) | `()` | stable |
| `ClientHandle::refresh_inline_values` | `workspace/inlineValue/refresh` | none (`null`) | `()` | stable |
| `ClientHandle::refresh_semantic_tokens` | `workspace/semanticTokens/refresh` | none (`null`) | `()` | stable |
| `ClientHandle::refresh_folding_ranges` | `workspace/foldingRange/refresh` | none (`null`) | `()` | stable |
| `ClientHandle::refresh_text_document_content` | `workspace/textDocumentContent/refresh` | `TextDocumentContentRefreshParams` | `()` | stable |
| `ClientHandle::begin_progress` | `window/workDoneProgress/create`, then `$/progress` begin | `ProgressOptions` | `ProgressHandle` | stable |
| `ProgressHandle::report` | `$/progress` report | `Option<String>` message, `Option<u32>` percentage | `()` | stable |
| `ProgressHandle::end` | `$/progress` end | `Option<String>` message | `()` | stable |
| `ServerContext::begin_progress` | `$/progress` begin on the request's `workDoneToken` | `ProgressOptions` | `Option<ProgressHandle>` | stable |
| `ServerContext::partial_results` | none; lends a borrowed sink | request marker `R` | `Option<PartialResultSink<'_, R>>` | stable |
| `PartialResultSink::report` | `$/progress` on the request's `partialResultToken` | `R`'s metaModel partial-result type | `()` | stable |

## What the helpers deliberately leave to you

The helpers are thin. None of the following exists, and none is planned as
implicit behavior:

- **No configuration cache.** `configuration` results go to the caller only;
  the `Workspace` snapshot updates solely through
  `workspace/didChangeConfiguration`.
- **No diagnostics cache.** `publish_diagnostics` sends params verbatim; the
  framework never stores, deduplicates, or clears diagnostics.
- **No dynamic-registration state.** The framework keeps no list of
  capabilities registered with the client; tracking what you registered is
  the application's job.
- **No implicit overload recovery.** The resource policy bounds queued message
  count and encoded bytes. When either budget is full, ordinary sends return
  `ClientError::OutboundOverloaded`; the application decides whether to retry
  or skip optional output.
- **No outgoing notebook method.** LSP defines notebook synchronization as
  client-to-server only, so the helper surface has nothing for it. Inbound
  notebook sync is a separate, supported surface — see
  [Notebook synchronization](./features-and-workspace.md#notebook-synchronization).
- **No implicit progress end.** Dropping a [`ProgressHandle`] removes its
  token with a warning but sends nothing; only `end` ends a progress.

[`ClientHandle`]: https://docs.rs/lspf/latest/lspf/struct.ClientHandle.html
[`ServerContext`]: https://docs.rs/lspf/latest/lspf/struct.ServerContext.html
[`ProgressHandle`]: https://docs.rs/lspf/latest/lspf/struct.ProgressHandle.html
[`PartialResultSink`]: https://docs.rs/lspf/latest/lspf/struct.PartialResultSink.html
[`PartialResultRequest`]: https://docs.rs/lspf/latest/lspf/trait.PartialResultRequest.html
[`lspf::types`]: https://docs.rs/lspf/latest/lspf/types/
