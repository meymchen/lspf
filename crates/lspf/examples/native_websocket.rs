//! Serve the shared example handlers to one native WebSocket client.
//!
//! Build and run with:
//!
//! ```text
//! cargo run -p lspf --example native_websocket \
//!   --no-default-features --features websocket
//! ```

mod shared;

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = shared::build().expect("the shared registrations are valid");
    let outcome = lspf::websocket(server, "127.0.0.1:9258").serve().await?;
    std::process::exit(outcome.code());
}
