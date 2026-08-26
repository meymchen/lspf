# Client lifecycle control stays on ServerHandle

Status: Accepted.

The cloneable `ServerHandle` owns the caller-facing Client lifecycle transitions because `ClientConnection::serve` must be driven concurrently and consumes the connection. Generic typed calls are accepted only while running; `shutdown` uses the shared request deadline and, after success, resolves pending work before `exit`, while `disconnect` skips wire lifecycle traffic and converges directly on the same close path. The private protocol session exposes only cloneable shutdown and close mechanics, so Client lifecycle ordering remains endpoint policy rather than leaking into the shared core.
