#[test]
fn core_dispatcher_routes_task_creation_through_runtime() {
    let source = include_str!("../src/dispatcher.rs");

    assert!(!source.contains("tokio::spawn"));
    assert!(!source.contains("tokio::task::spawn_local"));
    assert!(!source.contains("wasm_bindgen_futures::spawn_local"));
}
