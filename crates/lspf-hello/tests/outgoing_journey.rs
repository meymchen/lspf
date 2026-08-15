//! The verified 0.4 outgoing journey over a real stdio connection (issue #112).
//!
//! These tests drive the freshly built `lspf-hello` binary through the
//! outgoing-helper surface: a handler-issued configuration request and its
//! client response, a show-message notification, a workspace edit, a custom
//! outgoing request, dynamic registration, every stable workspace refresh, and
//! work-done progress — including the client-error, client-cancel, and
//! disconnect paths, none of which may stall the connection or leak request or
//! progress tokens.

mod common;

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};

use common::{read_framed, spawn_hello, write_framed};

/// The `initialize` request for the outgoing journey: no client capabilities
/// gate the server-to-client helpers, so a minimal set suffices.
fn initialize_request(id: i32) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "processId": null,
            "clientInfo": { "name": "outgoing-journey-test", "version": "0.1.0" },
            "rootUri": null,
            "capabilities": {},
        },
    })
}

async fn send(stdin: &mut ChildStdin, message: Value) {
    write_framed(stdin, message.to_string().as_bytes()).await;
}

/// The journey's messages arrive in one deterministic order — the Command
/// handler awaits each step before starting the next — so every frame must be
/// exactly what the test expects next.
async fn expect_request(stdout: &mut BufReader<ChildStdout>, method: &str) -> Value {
    let message = read_framed(stdout).await;
    assert_eq!(
        message["method"], method,
        "expected a {method} request, got {message}"
    );
    assert!(
        message.get("id").is_some(),
        "a request carries an id, got {message}"
    );
    message
}

async fn expect_notification(stdout: &mut BufReader<ChildStdout>, method: &str) -> Value {
    let message = read_framed(stdout).await;
    assert_eq!(
        message["method"], method,
        "expected a {method} notification, got {message}"
    );
    assert!(
        message.get("id").is_none(),
        "a notification carries no id, got {message}"
    );
    message
}

/// Answer a server-initiated request with a success result.
async fn answer(stdin: &mut ChildStdin, request: &Value, result: Value) {
    send(
        stdin,
        json!({ "jsonrpc": "2.0", "id": request["id"], "result": result }),
    )
    .await;
}

/// Answer a server-initiated request with a JSON-RPC error.
async fn answer_error(stdin: &mut ChildStdin, request: &Value, code: i32, message: &str) {
    send(
        stdin,
        json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "error": { "code": code, "message": message },
        }),
    )
    .await;
}

/// initialize + initialized, asserting the advertised Command list.
async fn start_session(stdin: &mut ChildStdin, stdout: &mut BufReader<ChildStdout>) {
    send(stdin, initialize_request(1)).await;
    let resp = read_framed(stdout).await;
    assert_eq!(resp["id"], 1);
    assert_eq!(
        resp["result"]["capabilities"]["executeCommandProvider"],
        json!({ "commands": [
            "lspf-hello.workspaceRoots",
            "lspf-hello.readFile",
            "lspf-hello.outgoingJourney",
            "lspf-hello.cancellableProgress",
        ] }),
        "the journey commands are advertised in registration order"
    );
    send(
        stdin,
        json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    )
    .await;
}

/// shutdown + exit, asserting the clean-exit code.
async fn finish_session(
    stdin: ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    child: &mut tokio::process::Child,
) {
    let mut stdin = stdin;
    send(
        &mut stdin,
        json!({ "jsonrpc": "2.0", "id": 99, "method": "shutdown" }),
    )
    .await;
    let resp = read_framed(stdout).await;
    assert_eq!(resp["id"], 99);
    assert_eq!(resp["result"], Value::Null);
    send(&mut stdin, json!({ "jsonrpc": "2.0", "method": "exit" })).await;
    drop(stdin);

    let exit_status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("server exited within 5s")
        .expect("wait succeeds");
    assert_eq!(
        exit_status.code(),
        Some(0),
        "clean shutdown exits with code 0"
    );
}

/// Trigger the outgoing journey against `file:///journey.txt`.
async fn trigger_journey(stdin: &mut ChildStdin, id: i32) {
    send(
        stdin,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "workspace/executeCommand",
            "params": {
                "command": "lspf-hello.outgoingJourney",
                "arguments": ["file:///journey.txt"],
            },
        }),
    )
    .await;
}

/// Drive the journey's refresh block: five stable refresh requests, each
/// answered with the `null` acknowledgement.
async fn answer_refreshes(stdin: &mut ChildStdin, stdout: &mut BufReader<ChildStdout>) {
    for method in [
        "workspace/codeLens/refresh",
        "workspace/diagnostic/refresh",
        "workspace/inlayHint/refresh",
        "workspace/inlineValue/refresh",
        "workspace/semanticTokens/refresh",
    ] {
        let request = expect_request(stdout, method).await;
        assert_eq!(request["params"], Value::Null, "refreshes take no params");
        answer(stdin, &request, Value::Null).await;
    }
}

/// Drive the journey's progress block: answer the create request, then assert
/// the begin, report, and end notifications.
async fn drive_progress(stdin: &mut ChildStdin, stdout: &mut BufReader<ChildStdout>) {
    let create = expect_request(stdout, "window/workDoneProgress/create").await;
    assert_eq!(create["params"], json!({ "token": 1 }));
    answer(stdin, &create, Value::Null).await;

    let begin = expect_notification(stdout, "$/progress").await;
    assert_eq!(
        begin["params"],
        json!({
            "token": 1,
            "value": {
                "kind": "begin",
                "title": "Outgoing journey",
                "cancellable": true,
                "message": "wrapping up",
                "percentage": 0,
            },
        })
    );
    let report = expect_notification(stdout, "$/progress").await;
    assert_eq!(
        report["params"],
        json!({
            "token": 1,
            "value": {
                "kind": "report",
                "cancellable": true,
                "message": "halfway",
                "percentage": 50,
            },
        })
    );
    let end = expect_notification(stdout, "$/progress").await;
    assert_eq!(
        end["params"],
        json!({ "token": 1, "value": { "kind": "end", "message": "done" } })
    );
}

/// The happy path: initialize, a handler-issued configuration request answered
/// by the client, show message, apply edit, a custom outgoing request, dynamic
/// registration, every stable refresh, begin/report/end progress, shutdown,
/// and exit — all without the server constructing raw JSON.
#[tokio::test]
async fn the_outgoing_journey_runs_over_stdio() {
    let mut child = spawn_hello();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    start_session(&mut stdin, &mut stdout).await;
    trigger_journey(&mut stdin, 2).await;

    // 1. The handler-issued configuration request.
    let request = expect_request(&mut stdout, "workspace/configuration").await;
    assert_eq!(request["params"]["items"][0]["section"], "lspf-hello");
    answer(
        &mut stdin,
        &request,
        json!([{ "greeting": "from the client" }]),
    )
    .await;

    // 2. The show-message notification reports the lookup's outcome.
    let message = expect_notification(&mut stdout, "window/showMessage").await;
    assert_eq!(message["params"]["type"], 3, "MessageType::INFO");
    assert_eq!(
        message["params"]["message"],
        r#"lspf-hello configuration: {"greeting":"from the client"}"#
    );

    // 3. The workspace edit goes out exactly as the server built it.
    let request = expect_request(&mut stdout, "workspace/applyEdit").await;
    assert_eq!(request["params"]["label"], "lspf-hello touch");
    assert_eq!(
        request["params"]["edit"]["changes"]["file:///journey.txt"][0]["newText"],
        "// touched by lspf-hello\n"
    );
    answer(&mut stdin, &request, json!({ "applied": true })).await;

    // 4. The custom outgoing request rides the same typed broker.
    let request = expect_request(&mut stdout, "lspf-hello/ping").await;
    assert_eq!(request["params"], "ping");
    answer(&mut stdin, &request, json!("pong")).await;

    // 5. Dynamic registration: an announcement, not a Router mutation.
    let request = expect_request(&mut stdout, "client/registerCapability").await;
    assert_eq!(
        request["params"]["registrations"][0]["method"],
        "workspace/didChangeWatchedFiles"
    );
    answer(&mut stdin, &request, Value::Null).await;

    // 6. Every stable workspace refresh, then 7. the progress lifecycle.
    answer_refreshes(&mut stdin, &mut stdout).await;
    drive_progress(&mut stdin, &mut stdout).await;

    // The Command result records every step's outcome.
    let resp = read_framed(&mut stdout).await;
    assert_eq!(resp["id"], 2);
    let steps: Vec<(String, String)> =
        serde_json::from_value(resp["result"].clone()).expect("the journey result is a step list");
    assert_eq!(
        steps,
        vec![
            (
                "configuration".to_string(),
                r#"{"greeting":"from the client"}"#.to_string()
            ),
            ("applyEdit".to_string(), "applied:true".to_string()),
            ("ping".to_string(), "pong".to_string()),
            ("registerCapability".to_string(), "registered".to_string()),
            ("codeLens".to_string(), "refreshed".to_string()),
            ("diagnostic".to_string(), "refreshed".to_string()),
            ("inlayHint".to_string(), "refreshed".to_string()),
            ("inlineValue".to_string(), "refreshed".to_string()),
            ("semanticTokens".to_string(), "refreshed".to_string()),
            ("progress".to_string(), "completed".to_string()),
        ]
    );

    finish_session(stdin, &mut stdout, &mut child).await;
}

/// The error path: a client that answers the configuration lookup and the
/// custom request with JSON-RPC errors and refuses the edit watches the rest
/// of the journey complete — every later request gets a fresh, strictly
/// increasing ID, so no errored exchange leaks a pending entry or a token.
#[tokio::test]
async fn client_errors_do_not_stall_the_journey() {
    let mut child = spawn_hello();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    start_session(&mut stdin, &mut stdout).await;
    trigger_journey(&mut stdin, 2).await;

    let mut request_ids = Vec::new();
    let mut track = |request: &Value| {
        let id = request["id"].as_i64().expect("a numeric request id");
        assert!(
            request_ids.last().is_none_or(|last| id > *last),
            "request IDs are fresh and strictly increasing, got {id} after {request_ids:?}"
        );
        request_ids.push(id);
    };

    // 1. The configuration lookup fails remotely; the journey continues.
    let request = expect_request(&mut stdout, "workspace/configuration").await;
    track(&request);
    answer_error(&mut stdin, &request, -32803, "configuration unsupported").await;

    // 2. The failure is what the show-message notification reports.
    let message = expect_notification(&mut stdout, "window/showMessage").await;
    let text = message["params"]["message"].as_str().unwrap();
    assert!(
        text.starts_with("lspf-hello configuration: unavailable ("),
        "the message reports the failure, got {text}"
    );

    // 3. The client refuses the edit; the refusal comes back verbatim.
    let request = expect_request(&mut stdout, "workspace/applyEdit").await;
    track(&request);
    answer(
        &mut stdin,
        &request,
        json!({ "applied": false, "failureReason": "read-only" }),
    )
    .await;

    // 4. The custom request also fails remotely.
    let request = expect_request(&mut stdout, "lspf-hello/ping").await;
    track(&request);
    answer_error(&mut stdin, &request, -32601, "no such method").await;

    // 5. Registration, the refreshes, and progress all still complete.
    let request = expect_request(&mut stdout, "client/registerCapability").await;
    track(&request);
    answer(&mut stdin, &request, Value::Null).await;
    answer_refreshes(&mut stdin, &mut stdout).await;
    drive_progress(&mut stdin, &mut stdout).await;

    let resp = read_framed(&mut stdout).await;
    assert_eq!(resp["id"], 2);
    let steps: Vec<(String, String)> =
        serde_json::from_value(resp["result"].clone()).expect("the journey result is a step list");
    assert!(
        steps[0].0 == "configuration" && steps[0].1.starts_with("error:"),
        "the configuration error is reported, got {steps:?}"
    );
    assert!(
        steps.iter().any(|step| step
            == &(
                "applyEdit".to_string(),
                "applied:false (read-only)".to_string()
            )),
        "the refused edit is reported verbatim, got {steps:?}"
    );
    assert!(
        steps
            .iter()
            .any(|step| step.0 == "ping" && step.1.starts_with("error:")),
        "the ping error is reported, got {steps:?}"
    );
    assert_eq!(
        steps.last(),
        Some(&("progress".to_string(), "completed".to_string())),
        "the progress lifecycle completes after the errors, got {steps:?}"
    );

    finish_session(stdin, &mut stdout, &mut child).await;
}

/// The cancellation path: the client cancels the cancellable progress through
/// `window/workDoneProgress/cancel`, the server observes the fired token and
/// ends the progress itself. A second run then reuses the connection — and a
/// stray cancel for the first, already-ended token is ignored — proving the
/// registry held no leaked token.
#[tokio::test]
async fn cancellable_progress_ends_when_the_client_cancels() {
    let mut child = spawn_hello();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    start_session(&mut stdin, &mut stdout).await;

    for (command_id, token) in [(2, 1), (3, 2)] {
        send(
            &mut stdin,
            json!({
                "jsonrpc": "2.0",
                "id": command_id,
                "method": "workspace/executeCommand",
                "params": { "command": "lspf-hello.cancellableProgress", "arguments": [] },
            }),
        )
        .await;

        let create = expect_request(&mut stdout, "window/workDoneProgress/create").await;
        assert_eq!(create["params"], json!({ "token": token }));
        answer(&mut stdin, &create, Value::Null).await;

        let begin = expect_notification(&mut stdout, "$/progress").await;
        assert_eq!(
            begin["params"],
            json!({
                "token": token,
                "value": {
                    "kind": "begin",
                    "title": "Cancellable demo",
                    "cancellable": true,
                },
            })
        );
        let report = expect_notification(&mut stdout, "$/progress").await;
        assert_eq!(
            report["params"],
            json!({
                "token": token,
                "value": { "kind": "report", "cancellable": true, "percentage": 0 },
            })
        );

        if token == 2 {
            // A stray cancel for the first token — ended by now — is ignored:
            // it produces no frame and no error on the wire.
            send(
                &mut stdin,
                json!({
                    "jsonrpc": "2.0",
                    "method": "window/workDoneProgress/cancel",
                    "params": { "token": 1 },
                }),
            )
            .await;
        }
        send(
            &mut stdin,
            json!({
                "jsonrpc": "2.0",
                "method": "window/workDoneProgress/cancel",
                "params": { "token": token },
            }),
        )
        .await;

        // Cancellation sends nothing by itself: the server ends the progress.
        let end = expect_notification(&mut stdout, "$/progress").await;
        assert_eq!(
            end["params"],
            json!({ "token": token, "value": { "kind": "end", "message": "cancelled" } })
        );
        let resp = read_framed(&mut stdout).await;
        assert_eq!(resp["id"], command_id);
        assert_eq!(resp["result"], json!([["outcome", "cancelled"]]));
    }

    finish_session(stdin, &mut stdout, &mut child).await;
}

/// The disconnect path: the client vanishes while the configuration request is
/// still pending. The session closes, the pending request resolves as
/// cancelled instead of leaking, and the process ends with the no-shutdown
/// exit code rather than hanging.
#[tokio::test]
async fn disconnect_with_a_pending_request_exits_the_server() {
    let mut child = spawn_hello();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stderr = child.stderr.take().unwrap();
    let logs = tokio::spawn(async move {
        let mut logs = String::new();
        stderr.read_to_string(&mut logs).await.expect("read stderr");
        logs
    });

    start_session(&mut stdin, &mut stdout).await;
    trigger_journey(&mut stdin, 2).await;

    // The handler-issued request is on the wire; the client never answers.
    let _request = expect_request(&mut stdout, "workspace/configuration").await;

    drop(stdin);
    let exit_status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("the server exits within 5s of the disconnect")
        .expect("wait succeeds");
    assert_eq!(
        exit_status.code(),
        Some(1),
        "a disconnect without shutdown exits with code 1"
    );

    let logs = logs.await.expect("the stderr reader finished");
    assert!(
        !logs.contains("panicked"),
        "the disconnect path completed without a panic, got {logs:?}"
    );
}
