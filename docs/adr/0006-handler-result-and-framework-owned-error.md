# Request handlers return `Result<Option<T>, LspError>`; notifications return `()`

Every request [[Handler]] on the `LanguageServer` trait returns
`Result<Option<T>, LspError>`, where `LspError` is a framework-owned
type that maps onto JSON-RPC / LSP error codes (`InternalError`,
`InvalidParams`, `ContentModified`, `RequestCancelled`, etc.).
Notification handlers return `()`; for the rare unrecoverable case, the
framework exposes `self.shutdown()` rather than overloading the return
type.

We rejected `Option<T>` on requests because it makes errors invisible:
a failed `text.parse()` quietly turns into "no hover available", and
the user spends hours discovering the silent failure. `Result` makes
the failure mode discoverable and lets idiomatic `?` chains flow
through every handler.

We rejected generic `Result<Option<T>, Self::Error>` because the
`where Self::Error: Into<LspError>` bound spreads through every example
and signature; the win — letting users avoid writing one
`impl From<MyError> for LspError` — is too small for the cost. We
rejected panic-as-error-on-wire as the *primary* mechanism; panic
catching belongs in a separate middleware layer that complements
`Result`, not replaces it.

The cost we accept: Rust's orphan rule means we can't ship
`impl<E: Error> From<E> for LspError`, so users will occasionally write
`.map_err(LspError::internal)` to bridge a foreign error type. We treat
that as acceptable friction; the no-macro stance (ADR 0004) precludes
a derive that would smooth it.
