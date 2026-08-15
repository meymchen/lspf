# Migrating to 0.4

The 0.4 release completes the outgoing client surface: the named
notification, window, workspace, registration, refresh, and progress helpers
described in the [outgoing client guide](./outgoing-client.md). Most of it is
purely additive. Two changes break compilation against 0.3, and both are
mechanical to fix.

## `Context::publish_diagnostics` returns a `Result`

The legacy convenience call on [`Context`] used to swallow enqueue failures —
they were logged and dropped. In 0.4 it forwards the client helper's result,
so the caller sees serialization and connection-close failures:

```diff
-ctx.publish_diagnostics(params); // failures were logged and dropped
+ctx.publish_diagnostics(params)?; // or handle the ClientError yourself
```

The new signature is:

```rust
# use std::str::FromStr;
# use lspf::types::{Diagnostic, PublishDiagnosticsParams, Uri};
# use lspf::{ClientError, Context};
# fn on_change(ctx: &Context) -> Result<(), ClientError> {
let params = PublishDiagnosticsParams {
    uri: Uri::from_str("file:///main.rs").unwrap(),
    diagnostics: vec![Diagnostic::default()],
    version: Some(7),
};
// The failure now reaches the caller. A failed publish never invalidates the
// handler, so best-effort callers may simply drop the error.
if let Err(error) = ctx.publish_diagnostics(params) {
    tracing::warn!(%error, "publish failed");
}
Ok(())
# }
```

If your handler genuinely does not care whether the notification went out,
`let _ = ctx.publish_diagnostics(params);` reproduces the old behavior
explicitly.

## `ClientError::Remote` carries the full `JsonRpcError`

A peer's JSON-RPC error response used to arrive reduced to its message. In
0.4 the variant carries the whole [`JsonRpcError`](lspf::JsonRpcError) —
code, message, and optional data — so matching on it changes shape:

```rust
# use lspf::types::ConfigurationParams;
# use lspf::{Client, ClientError};
# async fn load(client: &Client) -> Result<(), ClientError> {
match client.configuration(ConfigurationParams { items: vec![] }).await {
    Ok(values) => {
        let _ = values;
        Ok(())
    }
    Err(ClientError::Remote(remote)) => {
        // Code, message, and data all survive the trip.
        tracing::warn!(code = remote.code, message = %remote.message, "client refused");
        Ok(())
    }
    Err(error) => Err(error),
}
# }
```

## New and newly gated API

Everything else in 0.4 is additive: the named notification helpers
(`show_message`, `log_message`, `log_trace`, `telemetry_event`, `progress`),
the window and workspace request helpers (`show_document`,
`show_message_request`, `apply_edit`, `configuration`, `workspace_folders`),
dynamic registration (`register_capability`, `unregister_capability`), the
five stable workspace refresh helpers, the connection-scoped work-done
progress lifecycle (`begin_progress` returning a [`ProgressHandle`], with
client cancellation observed through its `CancellationToken`), and the
outbound queue depth warning
([`DEFAULT_OUTBOUND_WARNING_THRESHOLD`](lspf::DEFAULT_OUTBOUND_WARNING_THRESHOLD),
configurable per server). The two proposed refresh helpers —
`refresh_folding_ranges` and `refresh_text_document_content` — live behind
the non-default `proposed` Cargo feature; enabling it adds API without
changing advertised capabilities.

[`Context`]: https://docs.rs/lspf/latest/lspf/struct.Context.html
[`ProgressHandle`]: https://docs.rs/lspf/latest/lspf/struct.ProgressHandle.html
