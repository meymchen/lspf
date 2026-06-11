//! End-to-end smoke tests — drive `examples/hello` over stdio and assert
//! both the lifecycle responses (commit 1) and the outgoing
//! `publishDiagnostics` notification (commit 2).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};

fn hello_binary() -> PathBuf {
    let status = std::process::Command::new("cargo")
        .args(["build", "--example", "hello", "--quiet"])
        .status()
        .expect("cargo build --example hello failed to launch");
    assert!(status.success(), "cargo build --example hello failed");

    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target");
    p.push("debug");
    p.push("examples");
    p.push("hello");
    p
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
    let exe = hello_binary();
    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn hello");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // 1. initialize
    let params: Value = serde_json::from_str(include_str!("fixtures/initialize-params.json"))
        .expect("fixture parses");
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": params,
    });
    write_framed(&mut stdin, init.to_string().as_bytes()).await;

    let resp = read_framed(&mut stdout).await;
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    let caps = &resp["result"]["capabilities"];
    assert_eq!(
        caps["textDocumentSync"], 2,
        "TEXT_DOCUMENT_SYNC default should derive into wire value 2 (Incremental); got {caps}"
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
    let exe = hello_binary();
    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn hello");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // 1. initialize
    let params: Value = serde_json::from_str(include_str!("fixtures/initialize-params.json"))
        .expect("fixture parses");
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": params,
    });
    write_framed(&mut stdin, init.to_string().as_bytes()).await;
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

#[tokio::test]
async fn exit_without_shutdown_returns_code_1() {
    let exe = hello_binary();
    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn hello");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // initialize first (skip shutdown)
    let params: Value = serde_json::from_str(include_str!("fixtures/initialize-params.json"))
        .expect("fixture parses");
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": params,
    });
    write_framed(&mut stdin, init.to_string().as_bytes()).await;
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
