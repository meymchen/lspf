//! The verified 0.3 user journey over a real stdio connection (issue #82).
//!
//! One test drives the freshly built `lspf-hello` binary through a complete
//! session: initialize capabilities, `didOpen`/`didChange` document
//! synchronization, a typed completion round trip, Command dispatch, a
//! workspace-folder change observed by a later command, an unopened-file read
//! through the configured `OsFileProvider`, then `shutdown` and `exit`.

mod common;

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::process::{ChildStdin, ChildStdout};

use common::{read_framed, spawn_hello, write_framed};

/// The `initialize` request for the journey: two announced workspace folders
/// and a minimal client capability set.
fn initialize_request(id: i32) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "processId": null,
            "clientInfo": { "name": "journey-test", "version": "0.1.0" },
            "rootUri": null,
            "capabilities": {},
            "workspaceFolders": [
                { "uri": "file:///folder-a", "name": "A" },
                { "uri": "file:///folder-b", "name": "B" },
            ],
        },
    })
}

/// Read messages until the response whose `id` matches `id`, skipping any
/// notification that interleaves.
async fn read_response(stdout: &mut BufReader<ChildStdout>, id: i32) -> Value {
    loop {
        let message = read_framed(stdout).await;
        if message.get("id") == Some(&Value::from(id)) {
            return message;
        }
    }
}

/// An absolute `file:` URI for `path`, percent-encoding every byte that is
/// not legal in a URI path. Mirrors the framework's own test helper so the
/// journey exercises a realistic client spelling.
fn file_uri(path: &Path) -> String {
    let absolute = std::path::absolute(path).expect("the test path is absolute");
    let text = absolute
        .to_str()
        .expect("test paths are valid UTF-8")
        .replace('\\', "/");
    let mut encoded = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    // On Windows the drive letter is part of the path, so the URI needs the
    // empty authority spelled out as a third slash: `file:///C:/…`.
    #[cfg(windows)]
    let spelling = format!("file:///{encoded}");
    #[cfg(not(windows))]
    let spelling = format!("file://{encoded}");
    spelling
}

async fn send(stdin: &mut ChildStdin, message: Value) {
    write_framed(stdin, message.to_string().as_bytes()).await;
}

#[tokio::test]
async fn the_zero_three_journey_runs_over_stdio() {
    // An unopened file on disk that the server only ever reaches through its
    // configured `OsFileProvider`.
    let dir = tempfile::tempdir().expect("a tempdir is created");
    let file = dir.path().join("unopened.txt");
    std::fs::write(&file, "read from disk").expect("the test file writes");
    let file_uri = file_uri(&file);

    let mut child = spawn_hello();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // 1. initialize: the generated capabilities cover document sync, the
    //    typed features, the Commands in registration order, and multi-root
    //    workspace support — none of them handwritten in the server.
    eprintln!("[native-lifecycle] stage=initialize");
    send(&mut stdin, initialize_request(1)).await;
    let resp = read_response(&mut stdout, 1).await;
    assert_eq!(resp["jsonrpc"], "2.0");
    let caps = &resp["result"]["capabilities"];
    assert_eq!(caps["positionEncoding"], "utf-16");
    assert_eq!(caps["textDocumentSync"], 2, "engine-owned incremental sync");
    assert_eq!(caps["hoverProvider"], true);
    assert_eq!(
        caps["completionProvider"],
        json!({ "resolveProvider": true, "triggerCharacters": ["."] })
    );
    assert_eq!(
        caps["executeCommandProvider"],
        json!({ "commands": [
            "lspf-hello.workspaceRoots",
            "lspf-hello.readFile",
            "lspf-hello.outgoingJourney",
            "lspf-hello.cancellableProgress",
        ] }),
        "commands are advertised in registration order"
    );
    assert_eq!(
        caps["workspace"],
        json!({ "workspaceFolders": { "supported": true, "changeNotifications": true } })
    );

    // 2. initialized
    send(
        &mut stdin,
        json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    )
    .await;

    // 3. didOpen: the post-mutation hook observes the framework-opened
    //    document and publishes a diagnostic.
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///journey.txt",
                    "languageId": "plaintext",
                    "version": 1,
                    "text": "hello world\n",
                }
            },
        }),
    )
    .await;
    let notif = read_framed(&mut stdout).await;
    assert_eq!(notif["method"], "textDocument/publishDiagnostics");
    assert_eq!(notif["params"]["uri"], "file:///journey.txt");
    assert_eq!(notif["params"]["version"], 1);
    assert_eq!(
        notif["params"]["diagnostics"][0]["message"],
        "lspf saw this document open"
    );

    // 4. didChange: an incremental change the engine applies before any
    //    later handler reads the document.
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": "file:///journey.txt", "version": 2 },
                "contentChanges": [{
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 5 },
                    },
                    "text": "HELLO",
                }],
            },
        }),
    )
    .await;

    // 5. hover reads the synchronized document: "HELLO world\n", two words,
    //    version 2.
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": "file:///journey.txt" },
                "position": { "line": 0, "character": 0 },
            },
        }),
    )
    .await;
    let resp = read_response(&mut stdout, 3).await;
    assert_eq!(
        resp["result"]["contents"],
        json!({
            "kind": "markdown",
            "value": "`plaintext` · 2 words · version 2",
        }),
        "the hover handler read the framework-synchronized document, got {resp}"
    );

    // 6. completion: typed request, typed response.
    eprintln!("[native-lifecycle] stage=typed-request method=textDocument/completion");
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": "file:///journey.txt" },
                "position": { "line": 0, "character": 0 },
            },
        }),
    )
    .await;
    let resp = read_response(&mut stdout, 4).await;
    let items = resp["result"].as_array().expect("completion array");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["label"], "lspf-hello");
    assert_eq!(items[1]["label"], "workspaceRoots");

    // 7. completionItem/resolve: the dependent feature resolves one item.
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "completionItem/resolve",
            "params": { "label": "bare" },
        }),
    )
    .await;
    let resp = read_response(&mut stdout, 5).await;
    assert_eq!(resp["result"]["label"], "bare");
    assert_eq!(resp["result"]["detail"], "resolved by lspf-hello");

    // 8. Command dispatch: workspaceRoots reads the multi-root state
    //    announced at initialize.
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "workspace/executeCommand",
            "params": { "command": "lspf-hello.workspaceRoots", "arguments": [] },
        }),
    )
    .await;
    let resp = read_response(&mut stdout, 6).await;
    assert_eq!(
        resp["result"],
        json!([["file:///folder-a", "A"], ["file:///folder-b", "B"]])
    );

    // 9. A workspace-folder change: the engine mutates the connection's
    //    workspace before any later handler reads it.
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "workspace/didChangeWorkspaceFolders",
            "params": {
                "event": {
                    "added": [{ "uri": "file:///folder-c", "name": "C" }],
                    "removed": [{ "uri": "file:///folder-b", "name": "B" }],
                },
            },
        }),
    )
    .await;

    // 10. The same command now observes the mutated folders.
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "workspace/executeCommand",
            "params": { "command": "lspf-hello.workspaceRoots", "arguments": [] },
        }),
    )
    .await;
    let resp = read_response(&mut stdout, 7).await;
    assert_eq!(
        resp["result"],
        json!([["file:///folder-a", "A"], ["file:///folder-c", "C"]])
    );

    // 11. Unopened-file lookup: readFile resolves a URI the server has never
    //     seen through its OsFileProvider.
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "workspace/executeCommand",
            "params": { "command": "lspf-hello.readFile", "arguments": [file_uri] },
        }),
    )
    .await;
    let resp = read_response(&mut stdout, 8).await;
    assert_eq!(resp["result"], "read from disk");

    // 12. shutdown then exit: the server reports a clean ending.
    eprintln!("[native-lifecycle] stage=shutdown");
    send(
        &mut stdin,
        json!({ "jsonrpc": "2.0", "id": 9, "method": "shutdown" }),
    )
    .await;
    let resp = read_response(&mut stdout, 9).await;
    assert_eq!(resp["result"], Value::Null);

    eprintln!("[native-lifecycle] stage=exit");
    send(&mut stdin, json!({ "jsonrpc": "2.0", "method": "exit" })).await;
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
