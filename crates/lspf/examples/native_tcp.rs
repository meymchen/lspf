//! Serve the shared example handlers to one native TCP client.
//!
//! Build and run with:
//!
//! ```text
//! cargo run -p lspf --example native_tcp \
//!   --no-default-features --features tcp
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
    let outcome = lspf::tcp(server, "127.0.0.1:9257").serve().await?;
    std::process::exit(outcome.code());
}
