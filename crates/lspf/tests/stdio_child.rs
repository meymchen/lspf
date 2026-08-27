//! Real-process journeys for the Client stdio-child interface (issue #178).
//!
//! The tests cross only the public `ClientBuilder::spawn` seam. This test
//! executable re-launches itself as a small language server so every journey
//! uses OS pipes and a real child without adding a fixture binary to the crate.

use std::io::{BufRead, Write};
use std::process::Stdio;
use std::time::Duration;

use lspf::types::ClientCapabilities;
use lspf::{
    BuildError, ChildError, Client, ClientError, Outcome, ResourcePolicy, ResourcePolicyField,
};
use serde_json::{Value, json};
use tokio::process::Command;

const CHILD_MODE: &str = "LSPF_STDIO_CHILD_FIXTURE";

fn read_message(reader: &mut impl BufRead) -> Value {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0, "missing frame");
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            content_length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0; content_length.expect("Content-Length header")];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn write_message(writer: &mut impl Write, message: &Value) {
    let body = serde_json::to_vec(message).unwrap();
    write!(writer, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    writer.write_all(&body).unwrap();
    writer.flush().unwrap();
}

fn stdio_language_server_fixture() {
    let mode = std::env::var(CHILD_MODE).unwrap();
    let mut stdin = std::io::BufReader::new(std::io::stdin().lock());
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();

    let initialize = read_message(&mut stdin);
    stderr.write_all(&vec![b'x'; 256 * 1024]).unwrap();
    stderr.flush().unwrap();
    write_message(
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": initialize["id"],
            "result": { "capabilities": {} },
        }),
    );
    let initialized = read_message(&mut stdin);
    assert_eq!(initialized["method"], "initialized");
    if mode == "early-exit" {
        std::process::exit(23);
    }
    if mode == "shutdown-timeout" {
        ignore_terminate();
        let shutdown = read_message(&mut stdin);
        assert_eq!(shutdown["method"], "shutdown");
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    let shutdown = read_message(&mut stdin);
    assert_eq!(shutdown["method"], "shutdown");
    write_message(
        &mut stdout,
        &json!({ "jsonrpc": "2.0", "id": shutdown["id"], "result": null }),
    );
    let exit = read_message(&mut stdin);
    assert_eq!(exit["method"], "exit");
    std::process::exit(0);
}

fn fixture_command(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .env(CHILD_MODE, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[cfg(unix)]
fn ignore_terminate() {
    use std::os::raw::c_int;

    unsafe extern "C" {
        fn signal(signal: c_int, handler: usize) -> usize;
    }
    const SIGTERM: c_int = 15;
    const SIG_IGN: usize = 1;

    // SAFETY: POSIX `signal` receives the SIGTERM constant and the standard
    // SIG_IGN sentinel; no pointers are dereferenced.
    unsafe {
        signal(SIGTERM, SIG_IGN);
    }
}

#[cfg(windows)]
fn ignore_terminate() {}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    use std::os::raw::c_int;

    unsafe extern "C" {
        fn kill(pid: c_int, signal: c_int) -> c_int;
    }

    // SAFETY: signal 0 performs existence/permission checking without sending
    // a signal, and `pid` came from the spawned child.
    unsafe { kill(pid as c_int, 0) == 0 }
}

#[cfg(unix)]
async fn assert_reaped(pid: u32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while process_exists(pid) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the child PID disappears after cleanup");
}

#[cfg(unix)]
fn assert_reaped_blocking(pid: u32) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while process_exists(pid) {
        assert!(
            std::time::Instant::now() < deadline,
            "the child PID disappears after cleanup"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

async fn framed_exchange_drains_stderr_and_reclaims_a_successful_child() {
    let child = Client::builder(ClientCapabilities::default())
        .spawn(fixture_command("success"))
        .await
        .expect("the child initializes while its stderr pipe is drained");

    let output = child.shutdown().await.expect("graceful shutdown succeeds");

    assert_eq!(output.outcome(), Outcome::Exit { code: 0 });
    assert!(output.status().success());
    assert_eq!(output.stderr().len(), 64 * 1024);
    assert!(output.stderr_truncated());
}

async fn shutdown_timeout_terminates_then_kills_and_reaps_the_child() {
    let policy = ResourcePolicy {
        outbound_request_timeout: Some(Duration::from_millis(50)),
        ..ResourcePolicy::default()
    };
    let child = Client::builder(ClientCapabilities::default())
        .resource_policy(policy)
        .spawn(fixture_command("shutdown-timeout"))
        .await
        .expect("the stubborn child initializes");
    let pid = child.id();

    let error = child.shutdown().await.unwrap_err();

    assert!(matches!(error, ChildError::Lifecycle(ClientError::Timeout)));
    #[cfg(unix)]
    assert_reaped(pid).await;
}

async fn early_child_exit_reports_status_and_resolves_the_connection() {
    let child = Client::builder(ClientCapabilities::default())
        .spawn(fixture_command("early-exit"))
        .await
        .expect("the child completes initialize before exiting");

    let output = tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .expect("early exit resolves the Client connection")
        .expect("early exit is a terminal result");

    assert_eq!(output.outcome(), Outcome::TransportClosed);
    assert_eq!(output.status().code(), Some(23));
}

async fn dropping_a_connection_reclaims_its_child() {
    let child = Client::builder(ClientCapabilities::default())
        .spawn(fixture_command("shutdown-timeout"))
        .await
        .expect("the stubborn child initializes");
    let pid = child.id();

    drop(child);

    #[cfg(unix)]
    assert_reaped(pid).await;
}

async fn invalid_client_configuration_fails_before_spawning() {
    let policy = ResourcePolicy {
        max_inbound_requests: 0,
        ..ResourcePolicy::default()
    };
    let command = Command::new("lspf-command-that-must-not-exist");

    let result = Client::builder(ClientCapabilities::default())
        .resource_policy(policy)
        .spawn(command)
        .await;
    let error = match result {
        Ok(_) => panic!("invalid configuration must fail before spawning"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ChildError::Build(BuildError::InvalidResourcePolicy {
            field: ResourcePolicyField::MaxInboundRequests,
        })
    ));
}

async fn cancelling_a_terminal_future_still_reclaims_the_child() {
    let child = Client::builder(ClientCapabilities::default())
        .spawn(fixture_command("shutdown-timeout"))
        .await
        .expect("the stubborn child initializes");
    let pid = child.id();

    let cancelled = tokio::time::timeout(Duration::from_millis(50), child.wait()).await;

    assert!(cancelled.is_err());
    #[cfg(unix)]
    assert_reaped(pid).await;
}

fn main() {
    if std::env::var_os(CHILD_MODE).is_some() {
        stdio_language_server_fixture();
        return;
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        framed_exchange_drains_stderr_and_reclaims_a_successful_child().await;
        shutdown_timeout_terminates_then_kills_and_reaps_the_child().await;
        early_child_exit_reports_status_and_resolves_the_connection().await;
        dropping_a_connection_reclaims_its_child().await;
        invalid_client_configuration_fails_before_spawning().await;
        cancelling_a_terminal_future_still_reclaims_the_child().await;
    });

    let dropped_inside_pid = runtime.block_on(async {
        let child = Client::builder(ClientCapabilities::default())
            .spawn(fixture_command("shutdown-timeout"))
            .await
            .expect("the child initializes before drop inside block_on");
        let pid = child.id();
        drop(child);
        pid
    });
    drop(runtime);
    #[cfg(unix)]
    assert_reaped_blocking(dropped_inside_pid);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let child = runtime.block_on(async {
        Client::builder(ClientCapabilities::default())
            .spawn(fixture_command("shutdown-timeout"))
            .await
            .expect("the child initializes before its runtime stops")
    });
    let pid = child.id();
    drop(runtime);
    drop(child);
    #[cfg(unix)]
    assert!(
        !process_exists(pid),
        "drop without a runtime reaps the child"
    );
}
