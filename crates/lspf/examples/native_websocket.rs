//! Serve the shared example handlers to one native WebSocket client.
//!
//! Build and run with:
//!
//! ```text
//! cargo run -p lspf --example native_websocket \
//!   --no-default-features --features websocket
//! ```

mod example_logging;
mod shared;

#[tokio::main]
async fn main() -> lspf::Result<()> {
    // A socket adapter leaves stdout and stderr unused by the protocol, but a
    // client that did not spawn this process can still only observe it through
    // stderr. Without a subscriber installed here, `RUST_LOG` selects events
    // that nothing records.
    example_logging::init();
    let server = shared::build().expect("the shared registrations are valid");
    let outcome = lspf::websocket(server, "127.0.0.1:9258").serve().await?;
    std::process::exit(outcome.code());
}
