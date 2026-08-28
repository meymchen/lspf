use lspf::OsFileProvider;

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let outcome = lspf::stdio(lspf_markdown::server(OsFileProvider::new()))
        .serve()
        .await?;
    std::process::exit(outcome.code());
}
