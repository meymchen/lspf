//! ADR 0020: the protocol engine reaches an executor only through the internal
//! `Runtime` trait, so the native and WASM send models stay a compile-target
//! choice rather than a call site scattered through the core.

/// The engine owns every task the connection spawns, so it is the one module
/// that could reach an executor directly.
#[test]
fn the_protocol_engine_routes_task_creation_through_runtime() {
    let source = include_str!("../src/engine.rs");

    assert!(!source.contains("tokio::spawn"));
    assert!(!source.contains("tokio::task::spawn_local"));
    assert!(!source.contains("wasm_bindgen_futures::spawn_local"));
}
