//! Serve the shared example handlers to one native TCP client.
//!
//! Build and run with:
//!
//! ```text
//! cargo run -p lspf --example native_tcp \
//!   --no-default-features --features tcp
//! ```

mod shared;

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = shared::build().expect("the shared registrations are valid");
    let outcome = lspf::tcp(server, "127.0.0.1:9257").serve().await?;
    std::process::exit(outcome.code());
}
