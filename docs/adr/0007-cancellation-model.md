# Cancellation: future-drop plus explicit `CancellationToken`

When the client sends `$/cancelRequest`, the framework drops the
in-flight handler's future, which unwinds cleanly at the next `.await`
point. In addition, every request [[Handler]] receives a
`CancellationToken` parameter; CPU-bound handlers that don't yield often
can call `ct.is_cancelled()` between work units to bail early. The
client sees a `RequestCancelled` (-32800) JSON-RPC error on the wire for
each cancelled request.

For built-in document-aware handlers, the framework also auto-detects
stale work and short-circuits to `ContentModified` (-32801) when the
document version observed at the start of the handler differs from the
current version. User handlers opt into the same check via
`self.documents().check_version(&uri, version)?`.

We rejected drop-only cancellation because pure-CPU sections (parser,
regex, semantic analysis) don't yield until they finish, so a 200ms
computation can't be interrupted and cancellation effectively does
nothing where it matters most. We rejected an implicit task-local
token because the token's lifecycle differs between tokio and the WASM
executor, and a visible parameter is honest about request scope. We
rejected wrapping user handlers in auto-`ContentModified` detection
because the framework can't tell which document a handler "intended" to
operate on; the opt-in line keeps the magic explicit.

The cost we accept: every request handler signature carries a
`CancellationToken` parameter even when the handler ignores it. We
treat the explicitness as a feature, not noise.
