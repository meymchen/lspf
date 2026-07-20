# Ship our own `Layer` trait; don't depend on `tower`

Status: Accepted. Refined by
[ADR 0019](0019-protocol-invariants-and-service-layers.md).

lspf defines its own `Layer` and `Service` traits, narrowly shaped to LSP
dispatch (request → response future, notification → unit future). The
default `lspf::stdio()` / `tcp()` / `websocket()` constructors wrap the
user's `LanguageServer` in a built-in default stack — lifecycle, panic
catching, `$/cancelRequest` routing, and `tracing` spans — and the user
can call `.layer(MyLayer)` to push additional layers or
`.no_default_layers()` to start clean.

We rejected adopting `tower::Service` because tower's surface
(`poll_ready`, backpressure, `&mut self` call semantics, the cloning
dance for shared services) is shaped for HTTP middleware reuse we
don't need: LSP traffic does not flow through hyper, and the layers
we want are LSP-specific. Adopting tower would mean importing all of
its sharp edges in exchange for two pieces we'd build anyway
(`Layer` trait, `ServiceBuilder` syntax). We rejected an
extension-free design because the original brief asks for an
extensible server, and cross-cutting concerns (audit logging, rate
limits, request mirroring for debugging) cannot be retrofitted by
forking the framework. We rejected a fixed hook-set because two
independent crates that both want `on_request_start` can't both be
plugged in — hooks don't compose, layers do.

We accept that lspf diverges from the broader Rust async-server
ecosystem on this one trait shape. Users writing both an axum service
and an lspf server will encounter two `Layer` traits with similar
names. In return we get a dispatcher we control end-to-end — which
keeps the door open to a specialised default-stack fast path (planned
for v1.1, after we have benchmarks) that bypasses the generic
`Layer/Service` machinery for the common case.
