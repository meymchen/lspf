//! ADR 0020: the protocol engine reaches an executor only through the internal
//! `Runtime` trait, so the native and WASM send models stay a compile-target
//! choice rather than a call site scattered through the core.

use std::path::{Path, PathBuf};

/// Executor-specific spawn call sites. Allowed only inside the `Runtime`
/// implementations and the native Transport adapters (ADR 0020).
const SPAWN_CALLS: [&str; 3] = [
    "tokio::spawn",
    "tokio::task::spawn_local",
    "wasm_bindgen_futures::spawn_local",
];

/// Files allowed to reach an executor directly: the two `Runtime`
/// implementations live in `runtime.rs`, and stdio is the native Transport
/// adapter whose reader/writer tasks spawn on tokio.
const ALLOWED_FILES: [&str; 2] = ["runtime.rs", "stdio.rs"];

fn kernel_sources() -> Vec<PathBuf> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&src)
        .expect("the crate source directory is readable")
        .map(|entry| entry.expect("a source entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    for entry in std::fs::read_dir(src.join("transport"))
        .expect("the transport directory is readable")
        .map(|entry| entry.expect("a transport source entry"))
    {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files
}

/// The kernel sources with every test-only module removed: unit tests may
/// drive tokio directly, while the shipped kernel must not.
fn non_test_source(path: &Path) -> String {
    let source = std::fs::read_to_string(path).expect("the source is readable");
    source
        .split_once("mod tests")
        .map_or(source.as_str(), |(kernel, _)| kernel)
        .to_string()
}

#[test]
fn the_protocol_kernel_routes_task_creation_through_runtime() {
    for path in kernel_sources() {
        let file = path.file_name().and_then(|name| name.to_str()).unwrap();
        if ALLOWED_FILES.contains(&file) {
            continue;
        }
        let kernel = non_test_source(&path);
        for call in SPAWN_CALLS {
            assert!(
                !kernel.contains(call),
                "{file} reaches an executor directly ({call}) instead of routing through Runtime"
            );
        }
    }
}

#[test]
fn the_framework_never_fakes_send_or_sync() {
    for path in kernel_sources() {
        let file = path.file_name().and_then(|name| name.to_str()).unwrap();
        let source = std::fs::read_to_string(&path).expect("the source is readable");
        assert!(
            !source.contains("unsafe impl Send"),
            "{file} fakes Send for a framework type"
        );
        assert!(
            !source.contains("unsafe impl Sync"),
            "{file} fakes Sync for a framework type"
        );
    }
}
