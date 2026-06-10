# Framework state reaches handlers via a `Context` parameter

Every request [[Handler]] on the `LanguageServer` trait takes a
`ctx: &Context` parameter in addition to `&self`, the request payload,
and the [[CancellationToken]]. `Context` carries the [[Documents]]
handle, the workspace-folder cache, the outgoing helper methods
(`publish_diagnostics`, `show_message`, `apply_edit`, etc.), and the
request scope (id, tracing span). The user's struct holds only the
user's own state; framework state is never required to be a field of
the user struct.

We rejected a required `fn cx(&self) -> &Context` getter (option B):
every example would carry a line of pure ceremony, and forgetting to
store the field is a fresh class of bug. We rejected task-local
lookup (option C) because the magic — "where did `lspf::documents()`
come from? what happens if I call it outside a handler?" — costs more
than the saved keystrokes; it also breaks silently when users refactor
helpers out of handler bodies. We rejected the tower-lsp pattern of a
stored `Client` field (option D) because it conflates user state with
framework wiring and adds a constructor closure to the minimal `serve`
call.

The cost we accept: every handler signature carries a fourth parameter
even when it's unused, and `Context` will accumulate methods over time
that must be organised carefully. We treat the visibility of `ctx` as
a benefit — refactoring a handler into a free function is mechanical
because the dependencies are spelled out in the signature.
