//! Editor-facing journey through the packaged `lspf-markdown` stdio binary.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};

fn file_uri(path: &Path) -> String {
    let path = std::path::absolute(path)
        .expect("absolute fixture path")
        .to_string_lossy()
        .replace('\\', "/");
    let encoded = path.replace(' ', "%20");
    #[cfg(windows)]
    return format!("file:///{encoded}");
    #[cfg(not(windows))]
    format!("file://{encoded}")
}

async fn send(stdin: &mut ChildStdin, message: Value) {
    let body = message.to_string();
    stdin
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await
        .unwrap();
    stdin.write_all(body.as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();
}

async fn receive(stdout: &mut BufReader<ChildStdout>) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            assert!(stdout.read_line(&mut line).await.unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some(length) = line.strip_prefix("Content-Length: ") {
                content_length = Some(length.trim().parse::<usize>().unwrap());
            }
        }
        let mut body = vec![0; content_length.expect("Content-Length")];
        stdout.read_exact(&mut body).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    })
    .await
    .expect("server response")
}

async fn response(stdout: &mut BufReader<ChildStdout>, id: i32) -> Value {
    loop {
        let message = receive(stdout).await;
        if message["id"] == id {
            return message;
        }
    }
}

async fn run_session(workspace: &Path, exercise_features: bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lspf-markdown"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn packaged server binary");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let readme = file_uri(&workspace.join("readme.md"));
    let guide = file_uri(&workspace.join("guide.md"));

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "processId": null,
                "clientInfo": { "name": "editor-validation", "version": "1" },
                "rootUri": file_uri(workspace), "capabilities": {}
            }
        }),
    )
    .await;
    let initialized = response(&mut stdout, 1).await;
    assert_eq!(initialized["result"]["capabilities"]["textDocumentSync"], 2);
    assert_eq!(initialized["result"]["capabilities"]["hoverProvider"], true);
    assert!(initialized["result"]["capabilities"]["definitionProvider"].is_object());
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    )
    .await;

    if exercise_features {
        let original = "# Editor validation\n\n[missing](missing.md) and [guide](guide.md)\n";
        send(
            &mut stdin,
            json!({
                "jsonrpc":"2.0", "method":"textDocument/didOpen", "params":{
                    "textDocument":{"uri":readme,"languageId":"markdown","version":1,"text":original}
                }
            }),
        )
        .await;
        let diagnostics = receive(&mut stdout).await;
        assert_eq!(diagnostics["method"], "textDocument/publishDiagnostics");
        assert_eq!(
            diagnostics["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        send(
            &mut stdin,
            json!({
                "jsonrpc":"2.0", "method":"textDocument/didChange", "params":{
                    "textDocument":{"uri":readme,"version":2},
                    "contentChanges":[{"range":{"start":{"line":2,"character":10},"end":{"line":2,"character":20}},"text":"guide.md"}]
                }
            }),
        )
        .await;
        let diagnostics = receive(&mut stdout).await;
        assert!(
            diagnostics["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        send(
            &mut stdin,
            json!({"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"textDocument":{"uri":readme},"position":{"line":2,"character":36}}}),
        )
        .await;
        assert!(
            response(&mut stdout, 2).await["result"]["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("Validation guide")
        );

        send(
            &mut stdin,
            json!({"jsonrpc":"2.0","id":3,"method":"textDocument/definition","params":{"textDocument":{"uri":readme},"position":{"line":2,"character":36}}}),
        )
        .await;
        assert_eq!(response(&mut stdout, 3).await["result"]["uri"], guide);
    }

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":9,"method":"shutdown"}),
    )
    .await;
    assert_eq!(response(&mut stdout, 9).await["result"], Value::Null);
    send(&mut stdin, json!({"jsonrpc":"2.0","method":"exit"})).await;
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("clean shutdown timeout")
        .expect("wait for server");
    assert_eq!(status.code(), Some(0));
}

#[tokio::test]
async fn packaged_server_survives_an_editor_journey_and_restart() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("guide.md"), "# Validation guide\n").unwrap();
    std::fs::write(
        workspace.path().join("readme.md"),
        "fixture replaced by didOpen\n",
    )
    .unwrap();

    run_session(workspace.path(), true).await;
    run_session(workspace.path(), false).await;
}
