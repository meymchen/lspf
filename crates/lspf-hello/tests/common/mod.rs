//! Shared stdio-driving helpers for the `lspf-hello` integration tests.
//!
//! Both the smoke tests and the 0.3 journey test frame LSP messages over the
//! real binary's piped stdio; these helpers spawn it, frame outgoing bodies
//! with `Content-Length` headers, and parse incoming frames back.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};

pub(crate) fn hello_binary() -> PathBuf {
    // Cargo builds the binary target before running integration tests and
    // exposes its path through this env var, so the tests always drive the
    // freshly compiled `lspf-hello`.
    PathBuf::from(env!("CARGO_BIN_EXE_lspf-hello"))
}

/// The freshly built server with all three streams piped, ready to spawn.
///
/// `kill_on_drop` keeps a test that fails mid-session from leaving the process
/// behind; every stream is piped so nothing the server writes can reach the
/// test runner's own console. Returned unspawned for tests that also set
/// environment variables.
pub(crate) fn hello_command() -> Command {
    let mut command = Command::new(hello_binary());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

pub(crate) fn spawn_hello() -> tokio::process::Child {
    hello_command().spawn().expect("spawn hello")
}

/// Send one JSON body over the wire, framed with a `Content-Length` header.
pub(crate) async fn write_framed(stdin: &mut ChildStdin, body: &[u8]) {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes()).await.unwrap();
    stdin.write_all(body).await.unwrap();
    stdin.flush().await.unwrap();
}

/// Read one `Content-Length`-framed JSON body, so a hung server fails the
/// test instead of hanging CI.
pub(crate) async fn read_framed(stdout: &mut BufReader<ChildStdout>) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = stdout.read_line(&mut line).await.unwrap();
            assert!(n > 0, "server closed stdout before sending a header");
            if line == "\r\n" {
                break;
            }
            if let Some(rest) = line.strip_prefix("Content-Length: ") {
                content_length = Some(rest.trim().parse().unwrap());
            }
        }
        let length = content_length.expect("missing Content-Length header");
        let mut body = vec![0u8; length];
        stdout.read_exact(&mut body).await.unwrap();
        serde_json::from_slice(&body).expect("body is valid JSON")
    })
    .await
    .expect("a framed response arrived within 10s")
}
