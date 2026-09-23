//! The `--listen` mode a debugger launches: the binary serves one client that
//! connects over TCP instead of the client that spawned it.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;

async fn send(stream: &mut TcpStream, message: Value) {
    let body = message.to_string();
    stream
        .write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
        .await
        .unwrap();
}

async fn receive(reader: &mut BufReader<&mut TcpStream>) -> Value {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(length) = line.strip_prefix("Content-Length: ") {
            content_length = Some(length.trim().parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0; content_length.expect("Content-Length")];
    reader.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn listen_mode_serves_one_tcp_client_through_shutdown() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lspf-markdown"))
        .args(["--listen", "127.0.0.1:0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn lspf-markdown --listen");
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut announcement = String::new();
    tokio::time::timeout(Duration::from_secs(10), stderr.read_line(&mut announcement))
        .await
        .expect("the server announces its address")
        .unwrap();
    let address = announcement
        .trim()
        .strip_prefix("lspf-markdown listening on ")
        .unwrap_or_else(|| panic!("unexpected announcement {announcement:?}"))
        .to_string();

    let mut stream = TcpStream::connect(&address).await.unwrap();
    send(
        &mut stream,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"capabilities":{}}}),
    )
    .await;
    let initialized = tokio::time::timeout(
        Duration::from_secs(10),
        receive(&mut BufReader::new(&mut stream)),
    )
    .await
    .expect("initialize response");
    assert_eq!(initialized["id"], 1);
    assert!(initialized["result"]["capabilities"]["documentSymbolProvider"].is_object());

    send(
        &mut stream,
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    )
    .await;
    send(
        &mut stream,
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown"}),
    )
    .await;
    let shutdown = receive(&mut BufReader::new(&mut stream)).await;
    assert_eq!(shutdown["id"], 2);
    send(&mut stream, json!({"jsonrpc":"2.0","method":"exit"})).await;

    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("the server exits after exit")
        .unwrap();
    assert_eq!(status.code(), Some(0));
}

#[tokio::test]
async fn unknown_arguments_print_usage_and_fail() {
    let output = Command::new(env!("CARGO_BIN_EXE_lspf-markdown"))
        .arg("--bogus")
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "usage: lspf-markdown [--listen <host:port>]"
    );
}
