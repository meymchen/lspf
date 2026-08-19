//! The shared handler example (ADR 0020): one handler source compiles and
//! runs on both execution models.
//!
//! On native targets the same `Server` serves over stdio on `TokioRuntime`;
//! the identical registrations compile for `wasm32-unknown-unknown` as well,
//! where a browser or Node Worker host owns the connection. The framework runs
//! the handlers on `WasmRuntime`. No registration method, parameter, or return
//! shape forks between the two targets — only the internal task bounds
//! differ, expressed through the hidden `TaskSend` marker.
//!
//! ```text
//! cargo check -p lspf --example shared_server
//! cargo check -p lspf --example shared_server \
//!   --target wasm32-unknown-unknown --no-default-features --features wasm
//! ```

mod shared;

#[cfg(all(not(target_arch = "wasm32"), feature = "stdio"))]
use lspf::Server;

// `TaskSend` carries the whole target-dependent mobility difference: `Send`
// on native, nothing on wasm32. These assertions are compile-time evidence
// that the one registration source satisfies both targets' bounds.
const _: fn() = || {
    fn assert_task_send<T: lspf::TaskSend>() {}
    assert_task_send::<&str>();
    #[cfg(target_arch = "wasm32")]
    assert_task_send::<std::rc::Rc<()>>();
};

fn main() {
    let server = shared::build().expect("the static registrations are valid");

    // Native: serve over the stdio transport on the caller's Tokio runtime.
    #[cfg(all(not(target_arch = "wasm32"), feature = "stdio"))]
    serve_native(server);

    // Feature-matrix checks also compile every example for the TCP- and
    // WebSocket-only rows. Those rows have a runtime but deliberately do not
    // expose the stdio entry point, so the host-specific example stops here.
    #[cfg(all(not(target_arch = "wasm32"), not(feature = "stdio")))]
    drop(server);

    // WASM: the browser or Node host drives `Server::serve` over its
    // worker-channel transport; an example binary has no host, so it just
    // drops the server.
    #[cfg(target_arch = "wasm32")]
    drop(server);
}

#[cfg(all(not(target_arch = "wasm32"), feature = "stdio"))]
fn serve_native(server: Server<shared::State>) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a Tokio runtime starts");
    let outcome = runtime
        .block_on(lspf::stdio(server).serve())
        .expect("serving ends without a transport error");
    std::process::exit(outcome.code());
}
