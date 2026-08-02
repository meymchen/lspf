//! End-to-end smoke tests — drive the real `lspf-hello` binary over stdio and
//! assert the lifecycle responses, the outgoing `publishDiagnostics`
//! notification, a stdout stream carrying nothing but LSP frames, and the LSP
//! exit codes the binary maps its `Outcome` onto.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};

fn hello_binary() -> PathBuf {
    // Cargo builds the binary target before running integration tests and
    // exposes its path through this env var, so the test always drives the
    // freshly compiled `lspf-hello`.
    PathBuf::from(env!("CARGO_BIN_EXE_lspf-hello"))
}

/// The freshly built server with all three streams piped, ready to spawn.
///
/// `kill_on_drop` keeps a test that fails mid-session from leaving the process
/// behind; every stream is piped so nothing the server writes can reach the
/// test runner's own console. Returned unspawned for the one test that also
/// sets an environment variable.
fn hello_command() -> Command {
    let mut command = Command::new(hello_binary());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

/// Spawn the server, so a test only has to describe the session it drives.
fn spawn_hello() -> tokio::process::Child {
    hello_command().spawn().expect("spawn hello")
}

/// The `initialize` request the fixture describes — a real editor's payload.
fn initialize_request(id: i32) -> Value {
    let params: Value = serde_json::from_str(include_str!("fixtures/initialize-params.json"))
        .expect("fixture parses");
    json!({ "jsonrpc": "2.0", "id": id, "method": "initialize", "params": params })
}

async fn write_framed(stdin: &mut ChildStdin, body: &[u8]) {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes()).await.unwrap();
    stdin.write_all(body).await.unwrap();
    stdin.flush().await.unwrap();
}

async fn read_framed(stdout: &mut BufReader<ChildStdout>) -> Value {
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
}

#[tokio::test]
async fn lifecycle_round_trip() {
    let mut child = spawn_hello();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // 1. initialize
    write_framed(&mut stdin, initialize_request(1).to_string().as_bytes()).await;

    let resp = read_framed(&mut stdout).await;
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    let caps = &resp["result"]["capabilities"];
    assert_eq!(
        caps["textDocumentSync"], 2,
        "the engine owns document sync, so it must advertise wire value 2 \
         (Incremental) for the editor to send didOpen at all; got {caps}"
    );

    // 2. initialized
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {},
    });
    write_framed(&mut stdin, initialized.to_string().as_bytes()).await;

    // 3. shutdown
    let shutdown = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "shutdown",
    });
    write_framed(&mut stdin, shutdown.to_string().as_bytes()).await;

    let resp = read_framed(&mut stdout).await;
    assert_eq!(resp["id"], 2);
    assert_eq!(resp["result"], Value::Null);

    // 4. exit
    let exit = json!({
        "jsonrpc": "2.0",
        "method": "exit",
    });
    write_framed(&mut stdin, exit.to_string().as_bytes()).await;
    drop(stdin);

    let exit_status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("server exited within 5s")
        .expect("wait succeeds");
    assert_eq!(
        exit_status.code(),
        Some(0),
        "server should exit with code 0 after shutdown then exit"
    );
}

#[tokio::test]
async fn did_open_publishes_diagnostic() {
    let mut child = spawn_hello();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // 1. initialize
    write_framed(&mut stdin, initialize_request(1).to_string().as_bytes()).await;
    let _ = read_framed(&mut stdout).await;

    // 2. initialized
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {},
    });
    write_framed(&mut stdin, initialized.to_string().as_bytes()).await;

    // 3. didOpen
    let did_open = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": "file:///tmp/smoke.txt",
                "languageId": "plaintext",
                "version": 1,
                "text": "hello world\n",
            }
        },
    });
    write_framed(&mut stdin, did_open.to_string().as_bytes()).await;

    // 4. expect publishDiagnostics on the wire
    let notif = read_framed(&mut stdout).await;
    assert_eq!(notif["jsonrpc"], "2.0");
    assert_eq!(notif["method"], "textDocument/publishDiagnostics");
    let p = &notif["params"];
    assert_eq!(p["uri"], "file:///tmp/smoke.txt");
    assert_eq!(p["version"], 1);
    let diags = p["diagnostics"].as_array().expect("diagnostics array");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0]["source"], "lspf-hello");
    assert_eq!(diags[0]["severity"], 3); // Information
    assert_eq!(diags[0]["message"], "lspf saw this document open");

    // 5. shutdown + exit
    let shutdown = json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" });
    write_framed(&mut stdin, shutdown.to_string().as_bytes()).await;
    let _ = read_framed(&mut stdout).await;

    let exit = json!({ "jsonrpc": "2.0", "method": "exit" });
    write_framed(&mut stdin, exit.to_string().as_bytes()).await;
    drop(stdin);

    let exit_status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("server exited within 5s")
        .expect("wait succeeds");
    assert_eq!(exit_status.code(), Some(0));
}

/// Split the server's entire stdout into the JSON bodies of its
/// `Content-Length` frames, panicking on anything that is not part of a frame.
///
/// Deliberately stricter than [`read_framed`]: it consumes the whole stream, so
/// a log line, a `println!`, or a stray byte anywhere between frames — before
/// the first header, between a body and the next header, or after the last
/// body — fails the parse instead of being skipped.
fn parse_all_frames(stdout: &[u8]) -> Vec<Value> {
    const SEPARATOR: &[u8] = b"\r\n\r\n";
    const CONTENT_LENGTH: &str = "Content-Length: ";

    let mut rest = stdout;
    let mut bodies = Vec::new();
    while !rest.is_empty() {
        let separator = rest
            .windows(SEPARATOR.len())
            .position(|window| window == SEPARATOR)
            .unwrap_or_else(|| panic!("stdout has trailing bytes outside any frame: {rest:?}"));
        let header = std::str::from_utf8(&rest[..separator])
            .expect("a frame header is UTF-8; anything else is not LSP traffic");
        let length: usize = header
            .strip_prefix(CONTENT_LENGTH)
            .unwrap_or_else(|| panic!("stdout carries a non-LSP header block: {header:?}"))
            .trim()
            .parse()
            .expect("Content-Length is a number");
        let body_start = separator + SEPARATOR.len();
        let body = rest
            .get(body_start..body_start + length)
            .expect("the frame body is as long as its Content-Length claims");
        bodies.push(serde_json::from_slice(body).expect("a frame body is valid JSON"));
        rest = &rest[body_start + length..];
    }
    bodies
}

/// stdout is the LSP wire and nothing else: with tracing turned all the way up,
/// every byte the server writes there still belongs to a `Content-Length`
/// frame, and the diagnostics go to stderr.
#[tokio::test]
async fn stdout_carries_only_lsp_frames() {
    let mut child = hello_command()
        // Turn on the server's own logging, so a log line written to the wrong
        // stream would show up in this test rather than staying silent.
        .env("RUST_LOG", "lspf=trace,lspf_hello=trace")
        .spawn()
        .expect("spawn hello");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    // Drain stderr concurrently: a full pipe buffer would otherwise block the
    // server mid-session.
    let logs = tokio::spawn(async move {
        let mut logs = String::new();
        stderr.read_to_string(&mut logs).await.expect("read stderr");
        logs
    });

    for message in [
        initialize_request(1),
        json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///tmp/clean.txt",
                    "languageId": "plaintext",
                    "version": 1,
                    "text": "hello world\n",
                }
            },
        }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }),
        json!({ "jsonrpc": "2.0", "method": "exit" }),
    ] {
        write_framed(&mut stdin, message.to_string().as_bytes()).await;
    }
    drop(stdin);

    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stdout.read_to_end(&mut raw))
        .await
        .expect("stdout reached end of stream within 5s")
        .expect("read stdout");

    let frames = parse_all_frames(&raw);
    let methods: Vec<&str> = frames
        .iter()
        .filter_map(|frame| frame["method"].as_str())
        .collect();
    assert_eq!(
        methods,
        vec!["textDocument/publishDiagnostics"],
        "the only notification on the wire is the diagnostic, got {frames:#?}"
    );
    assert!(
        frames.iter().all(|frame| frame["jsonrpc"] == "2.0"),
        "every frame is a JSON-RPC 2.0 message, got {frames:#?}"
    );

    let logs = logs.await.expect("the stderr reader finished");
    assert!(
        logs.contains("lspf"),
        "the run produced no log output on stderr, so a clean stdout proves \
         nothing; got {logs:?}"
    );

    let exit_status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("server exited within 5s")
        .expect("wait succeeds");
    assert_eq!(exit_status.code(), Some(0));
}

#[tokio::test]
async fn exit_without_shutdown_returns_code_1() {
    let mut child = spawn_hello();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // initialize first (skip shutdown)
    write_framed(&mut stdin, initialize_request(1).to_string().as_bytes()).await;
    let _ = read_framed(&mut stdout).await;

    // exit without shutdown
    let exit = json!({ "jsonrpc": "2.0", "method": "exit" });
    write_framed(&mut stdin, exit.to_string().as_bytes()).await;
    drop(stdin);

    let exit_status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("server exited within 5s")
        .expect("wait succeeds");
    assert_eq!(exit_status.code(), Some(1));
}
