//! Serve the shared example handlers in a browser or Node Worker.
//!
//! The JavaScript host creates a `MessageChannel` and transfers one port to
//! the Worker. After loading the wasm-bindgen output, the Worker passes that
//! port to [`serve`]. Build the Rust half with:
//!
//! ```text
//! cargo build -p lspf --example worker_channel \
//!   --target wasm32-unknown-unknown --no-default-features \
//!   --features worker-channel
//! ```

mod shared;

use wasm_bindgen::prelude::*;
use web_sys::MessagePort;

/// Serve one transferred `MessagePort` until its LSP connection ends.
#[wasm_bindgen]
pub async fn serve(port: MessagePort) -> Result<i32, JsValue> {
    let server = shared::build()
        .map_err(|error| JsValue::from_str(&format!("invalid server registrations: {error}")))?;
    let outcome = lspf::worker_channel(server, port)
        .serve()
        .await
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    Ok(outcome.code())
}

fn main() {}
