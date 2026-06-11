use lspf::LanguageServer;

struct Hello;

impl LanguageServer for Hello {}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    lspf::stdio(Hello).serve().await
}
