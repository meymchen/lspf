# Single crate with feature flags

Status note:
The 0.5 Cargo feature graph (issue #126) ships this plan with the
runtime glue renamed `runtime-tokio` and the browser/Node Worker adapter as
the `worker-channel` feature, which implies `wasm`. Both hosts consume the
same `wasm32-unknown-unknown` artifact through target-specific wasm-bindgen
glue; CI builds both glue targets and runs the Node host through an LSP
startup/shutdown journey. Defaults pull in
`stdio`, which implies `runtime-tokio`; `tcp` and `websocket` do too,
and `websocket` additionally pulls `tokio-tungstenite`. The historical
body below is unchanged.

lspf ships as one crate, `lspf`, with optional feature flags for the
transports (`stdio`, `tcp`, `websocket`) and target glue (`tokio`,
`wasm`). Defaults pull in stdio + tokio. WASM users disable defaults
and enable `wasm` + whichever transports they need.

We rejected a multi-crate workspace with a thin `lspf` facade
(`lspf-core`, `lspf-tokio`, `lspf-wasm`, `lspf-websocket`) because the
facade buys nothing the user sees — they still write `lspf = "0.1"` —
while we pay maintenance cost on four crates' versions, READMEs, and
release tooling. We rejected one-crate-per-concern-no-facade because
it forces the user to assemble their own `Cargo.toml` from three or four
crates for the simplest server.

The cost we accept: if compile times become a problem (e.g.
`tokio-tungstenite` is heavy and slows down WASM-only users), we will
split out specific transports later. Feature flags can be tightened
without breaking the public API, but splitting crates after the fact is
a public-API change.
