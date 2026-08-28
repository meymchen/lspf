#[path = "soak_journeys/mod.rs"]
mod soak;

#[tokio::main]
async fn main() -> soak::SoakResult<()> {
    soak::run().await
}
