use lspf_markdown::OsFs;

const USAGE: &str = "usage: lspf-markdown [--listen <host:port>]";

/// How the server reaches its client.
#[derive(Debug, PartialEq, Eq)]
enum Transport {
    /// Serve the client that spawned this process over its standard streams.
    Stdio,
    /// Serve the first client that connects to this TCP address. A debugger
    /// can then own the process while an editor connects to it.
    Tcp(String),
}

fn transport(mut args: impl Iterator<Item = String>) -> Result<Transport, String> {
    match (args.next().as_deref(), args.next(), args.next()) {
        (None, _, _) => Ok(Transport::Stdio),
        (Some("--listen"), Some(address), None) => Ok(Transport::Tcp(address)),
        _ => Err(USAGE.to_string()),
    }
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = lspf_markdown::server(OsFs::new());
    let outcome = match transport(std::env::args().skip(1)) {
        Ok(Transport::Stdio) => lspf::stdio(server).serve().await?,
        Ok(Transport::Tcp(address)) => {
            lspf::tcp(server, address)
                .on_bound(|bound| eprintln!("lspf-markdown listening on {bound}"))
                .serve()
                .await?
        }
        Err(usage) => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    };
    std::process::exit(outcome.code());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Transport, String> {
        transport(args.iter().map(ToString::to_string))
    }

    #[test]
    fn arguments_select_the_transport() {
        assert_eq!(parse(&[]), Ok(Transport::Stdio));
        assert_eq!(
            parse(&["--listen", "127.0.0.1:9259"]),
            Ok(Transport::Tcp("127.0.0.1:9259".to_string()))
        );
        assert_eq!(parse(&["--listen"]), Err(USAGE.to_string()));
        assert_eq!(parse(&["--stdio"]), Err(USAGE.to_string()));
        assert_eq!(parse(&["--listen", "a", "b"]), Err(USAGE.to_string()));
    }
}
