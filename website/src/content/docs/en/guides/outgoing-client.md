---
title: Call the editor
description: Send typed notifications and requests from a server to its connected client.
---

This guide covers the server-to-client half of a connection: the typed
[`ClientHandle`] a handler reaches through [`ServerContext`], the named helpers
it offers for every standard outgoing notification and request, dynamic
capability registration, work-done progress, partial-result reporting, and the
escape hatches for custom methods. Nothing here is handwritten JSON — every
helper takes native [`lspf::types`] parameters and returns native results, and
every wire shape is pinned by a fixture under
`crates/lspf/tests/fixtures`. Every example here compiles as a doctest against
the shipped crate. The complete journey — configuration lookup, workspace
edit, custom outgoing requests, dynamic registration, every stable refresh,
disconnect handling, and cancellable progress — is covered at the public
interface by
[`outgoing_requests.rs`](https://github.com/meymchen/lspf/blob/main/crates/lspf/tests/outgoing_requests.rs)
and
[`progress.rs`](https://github.com/meymchen/lspf/blob/main/crates/lspf/tests/progress.rs).

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

Continue with [Progress and custom messages](progress-and-custom-messages) for long-running work, partial results, and protocol extensions.

[`ClientHandle`]: https://docs.rs/lspf/latest/lspf/struct.ClientHandle.html
[`ServerContext`]: https://docs.rs/lspf/latest/lspf/struct.ServerContext.html
[`lspf::types`]: https://docs.rs/lspf/latest/lspf/types/
