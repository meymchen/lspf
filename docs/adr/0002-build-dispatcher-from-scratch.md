# Build the dispatcher from scratch

lspf implements its own message-routing core rather than depending on an
existing Rust LSP framework crate (notably `async-lsp` or wrapping
`lsp-server`). Transport, executor, and middleware are three independent
traits we own.

The obvious move is to build on `async-lsp` — it already has a tower-based
middleware stack, a `LanguageServer` trait, and a closure-builder router.
But it ties itself to `AsyncRead + AsyncWrite` byte streams with LSP
`Content-Length` framing (which makes WebSocket transport awkward — the
framing is dead weight inside a message-framed channel), depends on tokio
in ways that block browser-WASM use, and pays tower's `Box<dyn>` and
`ControlFlow` dispatch cost on every message. Of our four upstream
constraints (WASM, WebSocket, async-only, extreme perf) it covers two
cleanly. Forking it would mean rewriting the transport layer and the
runtime glue while inheriting an API surface we don't get to shape.

So we accept the cost of writing our own message loop, framing, and
routing primitives in exchange for: a transport trait that treats
WebSocket as a first-class message channel (no `Content-Length`), a
runtime trait that lets the WASM target swap out the tokio assumptions,
and a routing path we can keep allocation-free where it matters.
