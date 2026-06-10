# Async-only runtime

lspf supports only an async execution model. There is no sync mode, no
`std::thread`-based event loop, no `block_on` shim hiding inside the
dispatcher. Every handler is `async fn`; the framework runs on tokio
natively and `wasm-bindgen-futures` in browser WASM.

We chose this because the project's other constraints — browser-WASM
support, WebSocket transport, and a single well-tuned core for "extreme
performance" — make a parallel sync runtime expensive to maintain and
unable to share its hot path with the async one. Browser WASM has no
blocking I/O, so a sync core could not target it anyway; supporting both
would mean two cores, two transport stacks, and two sets of bugs for a
small ergonomic gain.

The cost we accept: every user must pull in an async executor, and
contributors must reason about `Send`, `'static`, and `Pin<Box<dyn
Future>>` in the dispatcher. The most common Rust LSP base crate
(`lsp-server`) is sync, so newcomers may expect a sync option and need to
be told why we don't ship one.
