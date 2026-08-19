# Transport is message-framed; v1 ships four adapters

Status note:
The 0.5 implementation retains this message-framed contract and its four v1
adapters while revising the Rust ownership surface: `Transport::split`
transfers a `TransportReader::recv` half and a `TransportWriter` half with
`send` and consuming `shutdown` to the engine's independent loops. Custom
adapters are passed directly to `Server::serve`. This supersedes the trait
sketch and `from_transport` name in the historical body below.
[ADR 0020](0020-runtime-and-native-wasm-send-model.md) updates the execution
constraints: the `Send + 'static` supertraits apply to the native Transport
and both halves only; on `wasm32` the worker-channel values are not `Send`.

The historical `Transport` trait sketch was message-framed:

```rust
pub trait Transport: Send + 'static {
    async fn recv(&mut self) -> Result<RawMessage, TransportError>;
    async fn send(&mut self, msg: RawMessage) -> Result<(), TransportError>;
    async fn shutdown(self) -> Result<(), TransportError>;
}
```

The dispatcher and every [[Layer]] see one JSON-RPC envelope per call.
Stdio and TCP adapters add/strip `Content-Length: N\r\n\r\n` framing
internally; WebSocket and worker-channel adapters carry one envelope per
frame natively. v1 ships four adapters: `lspf::stdio()` (native +
tokio), `lspf::tcp(addr)` (native + tokio), `lspf::websocket(addr)`
(native + tokio + tokio-tungstenite), and `lspf::worker_channel(port)`
(WASM + wasm-bindgen-futures, wrapping a JS `MessagePort`).

We rejected a byte-stream `AsyncRead + AsyncWrite` transport shape
because every dispatcher path would have to know about LSP framing and
the WebSocket and worker-channel adapters would have to fake a byte
stream they don't have. We rejected shipping a TLS-enabled
`lspf::tcp_tls(...)` because TLS configuration (certificate stores,
mTLS, ALPN, rotation) is its own deep topic that either oversimplifies
in the API or balloons it — users wrap their own `rustls` / native-tls
stream and feed it through a `lspf::from_transport(impl Transport)`
escape hatch. We rejected supporting in-browser WebSocket clients
connecting *out* to a remote server (the `web-sys::WebSocket` path)
because the WASM-in-Worker channel covers the common Monaco / Theia-web
integration; users with the niche outbound case implement `Transport`
themselves. We rejected multi-connection serving in v1 because per-LSP
the model is one server per client/workspace, and a multi-connection
mode introduces shared-state and isolation questions without a clear
single answer.

The cost we accept: users who want TLS-on-TCP write a few lines of
boilerplate, and users who eventually need multi-connection serving
will run multiple processes (or wait for v2).
