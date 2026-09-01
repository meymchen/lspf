# Outgoing client helpers

This guide covers the server-to-client half of a connection: the typed
[`ClientHandle`] a handler reaches through [`ServerContext`], the named helpers
it offers for every standard outgoing notification and request, dynamic
capability registration, work-done progress, and the escape hatches for
custom methods. Nothing here is handwritten JSON — every helper takes native
`lsp-types` parameters and returns native results, and every wire shape is
pinned by a fixture under `crates/lspf/tests/fixtures`. Every example here
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

Five helpers ask the client to re-pull a stable workspace feature —
[`code_lens_refresh`](lspf::ClientHandle::code_lens_refresh),
[`diagnostic_refresh`](lspf::ClientHandle::diagnostic_refresh),
[`inlay_hint_refresh`](lspf::ClientHandle::inlay_hint_refresh),
[`inline_value_refresh`](lspf::ClientHandle::inline_value_refresh), and
[`semantic_tokens_refresh`](lspf::ClientHandle::semantic_tokens_refresh). Each
takes no parameters and returns the client's `null` acknowledgement as `()`;
the helper owns no recomputation policy, and the framework keeps no lens,
diagnostic, hint, value, or token state for it to touch.

With the non-default `proposed` Cargo feature,
[`refresh_folding_ranges`](lspf::ClientHandle::refresh_folding_ranges) and
[`refresh_text_document_content`](lspf::ClientHandle::refresh_text_document_content)
join the surface, using request markers from
[`lspf::proposed`](lspf::proposed) because `lsp-types` 0.97.x lacks them.
Enabling the feature only adds API; it never changes the advertised
capabilities. The default surface contains no proposed, draft, or notebook
method.

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
| `ClientHandle::apply_edit` | `workspace/applyEdit` | `ApplyWorkspaceEditParams` | `ApplyWorkspaceEditResponse` | stable |
| `ClientHandle::configuration` | `workspace/configuration` | `ConfigurationParams` | `Vec<serde_json::Value>` | stable |
| `ClientHandle::workspace_folders` | `workspace/workspaceFolders` | none (`null`) | `Option<Vec<WorkspaceFolder>>` | stable |
| `ClientHandle::register_capability` | `client/registerCapability` | `RegistrationParams` | `()` | stable |
| `ClientHandle::unregister_capability` | `client/unregisterCapability` | `UnregistrationParams` | `()` | stable |
| `ClientHandle::code_lens_refresh` | `workspace/codeLens/refresh` | none (`null`) | `()` | stable |
| `ClientHandle::diagnostic_refresh` | `workspace/diagnostic/refresh` | none (`null`) | `()` | stable |
| `ClientHandle::inlay_hint_refresh` | `workspace/inlayHint/refresh` | none (`null`) | `()` | stable |
| `ClientHandle::inline_value_refresh` | `workspace/inlineValue/refresh` | none (`null`) | `()` | stable |
| `ClientHandle::semantic_tokens_refresh` | `workspace/semanticTokens/refresh` | none (`null`) | `()` | stable |
| `ClientHandle::refresh_folding_ranges` | `workspace/foldingRange/refresh` | none (`null`) | `()` | proposed |
| `ClientHandle::refresh_text_document_content` | `workspace/textDocumentContent/refresh` | `TextDocumentContentRefreshParams` | `()` | proposed |
| `ClientHandle::begin_progress` | `window/workDoneProgress/create`, then `$/progress` begin | `ProgressOptions` | `ProgressHandle` | stable |
| `ProgressHandle::report` | `$/progress` report | `Option<String>` message, `Option<u32>` percentage | `()` | stable |
| `ProgressHandle::end` | `$/progress` end | `Option<String>` message | `()` | stable |

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
- **No notebook support.** The helper surface contains no notebook method.
- **No implicit progress end.** Dropping a [`ProgressHandle`] removes its
  token with a warning but sends nothing; only `end` ends a progress.

[`ClientHandle`]: https://docs.rs/lspf/latest/lspf/struct.ClientHandle.html
[`ServerContext`]: https://docs.rs/lspf/latest/lspf/struct.ServerContext.html
[`ProgressHandle`]: https://docs.rs/lspf/latest/lspf/struct.ProgressHandle.html
