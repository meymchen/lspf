# v1 scope: server-only, no notebooks, pygls-equivalent helper coverage

For v1, lspf builds **language servers only**. The framework does not
expose APIs for building LSP clients, does not implement notebook
document support, and does not include built-in metrics — observability
goes through the `tracing` crate and users plug their own subscriber.

On the server side, lspf ships pygls-equivalent built-in coverage: all
of pygls's outgoing notifications (`publish_diagnostics`, `show_message`,
`log_message`, `log_trace`, `telemetry`, `progress`) and all of its
outgoing requests (`show_document`, `show_message_request`, `apply_edit`,
`fetch_configuration`, `workspace_folders`, `register_capability`,
`unregister_capability`, and the six `workspace/*/refresh` methods),
plus one piece of sugar on top of `progress` for the work-done lifecycle
(`begin_progress(title) → handle.report() → handle.end()`).

We rejected including a client framework in v1 because its concurrency
shape (driving a remote server, awaiting responses) differs enough from
the server shape that bundling them doubles the trait surface and forces
design choices on a use case nobody has named yet; if demand appears,
`lspf-client` ships separately and reuses the transport and JSON-RPC
layers. We rejected notebook documents because they bring cell and
kernel concepts that bloat the document model for users who will never
touch them. We rejected built-in metrics because `tracing` events cover
counts and durations, and Prometheus-style metrics belong to whatever
observability stack the user already runs.

The cost we accept: a client crate added later will need to reuse but
not impose on the server's trait surface, which is a small constraint
on how `Transport` and the JSON-RPC framing primitives are factored.
