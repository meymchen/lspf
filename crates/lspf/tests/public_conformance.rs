//! Downstream-only Server and Client conformance journeys (issue #179).
//!
//! As an integration-test executable, this crate can name only lspf's public
//! surface. The custom loopback uses the documented Transport contract; the
//! stdio half re-launches this executable as a real language-server child.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lspf::types::ClientCapabilities;
use lspf::types::request::Request;
use lspf::{
    Client, ClientError, LspError, Outcome, RawMessage, Server, ServerContext, ServerHandle,
    Transport, TransportError, TransportReader, TransportWriter,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

const WATCHDOG: Duration = Duration::from_secs(5);
const CHILD_MODE: &str = "LSPF_PUBLIC_CONFORMANCE_CHILD";

#[derive(Default)]
struct ConformanceState {
    cancellations: AtomicUsize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct EchoParams {
    text: String,
    delay_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct EchoResult {
    text: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct EmptyParams {}

enum ServerEcho {}

impl Request for ServerEcho {
    type Params = EchoParams;
    type Result = EchoResult;
    const METHOD: &'static str = "conformance/serverEcho";
}

enum ClientEcho {}

impl Request for ClientEcho {
    type Params = EchoParams;
    type Result = EchoResult;
    const METHOD: &'static str = "conformance/clientEcho";
}

enum ReversePair {}

impl Request for ReversePair {
    type Params = EmptyParams;
    type Result = Vec<EchoResult>;
    const METHOD: &'static str = "conformance/reversePair";
}

enum WaitForCancellation {}

impl Request for WaitForCancellation {
    type Params = EmptyParams;
    type Result = ();
    const METHOD: &'static str = "conformance/waitForCancellation";
}

enum NeverRespond {}

impl Request for NeverRespond {
    type Params = EmptyParams;
    type Result = ();
    const METHOD: &'static str = "conformance/neverRespond";
}

enum CancellationCount {}

impl Request for CancellationCount {
    type Params = EmptyParams;
    type Result = usize;
    const METHOD: &'static str = "conformance/cancellationCount";
}

enum ExitWithoutResponse {}

impl Request for ExitWithoutResponse {
    type Params = EmptyParams;
    type Result = ();
    const METHOD: &'static str = "conformance/exitWithoutResponse";
}

type Incoming = Result<RawMessage, TransportError>;

struct LoopbackTransport {
    incoming: mpsc::UnboundedReceiver<Incoming>,
    outgoing: mpsc::UnboundedSender<Incoming>,
}

struct LoopbackReader(mpsc::UnboundedReceiver<Incoming>);
struct LoopbackWriter(mpsc::UnboundedSender<Incoming>);

impl Transport for LoopbackTransport {
    type Reader = LoopbackReader;
    type Writer = LoopbackWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (LoopbackReader(self.incoming), LoopbackWriter(self.outgoing))
    }
}

impl TransportReader for LoopbackReader {
    async fn recv(&mut self) -> Incoming {
        self.0.recv().await.unwrap_or(Err(TransportError::Closed))
    }
}

impl TransportWriter for LoopbackWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        self.0.send(Ok(message)).map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

struct LoopbackPair {
    server: LoopbackTransport,
    client: LoopbackTransport,
    fail_client_reader: mpsc::UnboundedSender<Incoming>,
}

fn loopback_pair() -> LoopbackPair {
    let (to_server, server_incoming) = mpsc::unbounded_channel();
    let (to_client, client_incoming) = mpsc::unbounded_channel();
    LoopbackPair {
        server: LoopbackTransport {
            incoming: server_incoming,
            outgoing: to_client.clone(),
        },
        client: LoopbackTransport {
            incoming: client_incoming,
            outgoing: to_server,
        },
        fail_client_reader: to_client,
    }
}

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(WATCHDOG, future)
        .await
        .expect("public conformance operation completes within the watchdog")
}

fn conformance_server(
    server_completions: mpsc::UnboundedSender<String>,
    cancellation_entered: mpsc::UnboundedSender<()>,
    cancellation_observed: mpsc::UnboundedSender<()>,
    pending_entered: mpsc::UnboundedSender<()>,
) -> Server<ConformanceState> {
    Server::builder(ConformanceState::default())
        .request::<ServerEcho, _, _>(move |_state, _ctx, params, _cancellation| {
            let completions = server_completions.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(params.delay_ms)).await;
                completions.send(params.text.clone()).unwrap();
                Ok(EchoResult {
                    text: format!("server: {}", params.text),
                })
            }
        })
        .request::<ReversePair, _, _>(
            |_state: Arc<ConformanceState>, ctx: ServerContext, _params, _cancellation| async move {
                let client = ctx.client();
                let first = client.request::<ClientEcho>(EchoParams {
                    text: "first".into(),
                    delay_ms: 60,
                });
                let second = client.request::<ClientEcho>(EchoParams {
                    text: "second".into(),
                    delay_ms: 5,
                });
                let (first, second) = tokio::join!(first, second);
                Ok(vec![
                    first.map_err(LspError::internal)?,
                    second.map_err(LspError::internal)?,
                ])
            },
        )
        .request::<WaitForCancellation, _, _>(move |state, _ctx, _params, cancellation| {
            let entered = cancellation_entered.clone();
            let observed = cancellation_observed.clone();
            async move {
                entered.send(()).unwrap();
                cancellation.cancelled().await;
                state.cancellations.fetch_add(1, Ordering::SeqCst);
                observed.send(()).unwrap();
                Err(LspError::RequestCancelled)
            }
        })
        .request::<NeverRespond, _, _>(move |_state, _ctx, _params, _cancellation| {
            let entered = pending_entered.clone();
            async move {
                entered.send(()).unwrap();
                std::future::pending().await
            }
        })
        .request::<CancellationCount, _, _>(|state, _ctx, _params, _cancellation| async move {
            Ok(state.cancellations.load(Ordering::SeqCst))
        })
        .request::<ExitWithoutResponse, _, _>(|_state, _ctx, _params, _cancellation| async move {
            std::process::exit(23)
        })
        .build()
        .expect("the public conformance Server builds")
}

async fn connect_loopback(
    pair: LoopbackPair,
    server: Server<ConformanceState>,
    client_completions: mpsc::UnboundedSender<String>,
) -> (
    ServerHandle,
    tokio::task::JoinHandle<lspf::Result<Outcome>>,
    tokio::task::JoinHandle<lspf::Result<Outcome>>,
    mpsc::UnboundedSender<Incoming>,
) {
    let server_serving = tokio::spawn(server.serve(pair.server));
    let client = Client::builder(ClientCapabilities::default())
        .request::<ClientEcho, _, _>(move |_ctx, params, _cancellation| {
            let completions = client_completions.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(params.delay_ms)).await;
                completions.send(params.text.clone()).unwrap();
                Ok(EchoResult {
                    text: format!("client: {}", params.text),
                })
            }
        })
        .build(pair.client)
        .expect("the public conformance Client builds");
    let connection = within(client.connect())
        .await
        .expect("the public conformance Client initializes");
    let server_handle = connection.server();
    let client_serving = tokio::spawn(connection.serve());
    (
        server_handle,
        server_serving,
        client_serving,
        pair.fail_client_reader,
    )
}

async fn wait_for_cancellation_count(server: &ServerHandle) {
    within(async {
        loop {
            if server
                .request::<CancellationCount>(EmptyParams::default())
                .await
                .expect("the cancellation probe completes")
                == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

async fn symmetric_loopback_journey() {
    let (server_completions, mut server_completion_rx) = mpsc::unbounded_channel();
    let (client_completions, mut client_completion_rx) = mpsc::unbounded_channel();
    let (cancellation_entered, mut cancellation_entered_rx) = mpsc::unbounded_channel();
    let (cancellation_observed, mut cancellation_observed_rx) = mpsc::unbounded_channel();
    let (pending_entered, _pending_entered_rx) = mpsc::unbounded_channel();
    let server = conformance_server(
        server_completions,
        cancellation_entered,
        cancellation_observed,
        pending_entered,
    );
    let (server, server_serving, client_serving, _fail_client_reader) =
        connect_loopback(loopback_pair(), server, client_completions).await;

    let first = server.request::<ServerEcho>(EchoParams {
        text: "first".into(),
        delay_ms: 60,
    });
    let second = server.request::<ServerEcho>(EchoParams {
        text: "second".into(),
        delay_ms: 5,
    });
    let (first, second) = within(async { tokio::join!(first, second) }).await;
    assert_eq!(
        first
            .expect("the first Server request stays correlated")
            .text,
        "server: first"
    );
    assert_eq!(
        second
            .expect("the second Server request stays correlated")
            .text,
        "server: second"
    );
    assert_eq!(server_completion_rx.recv().await.as_deref(), Some("second"));
    assert_eq!(server_completion_rx.recv().await.as_deref(), Some("first"));

    let reverse = within(server.request::<ReversePair>(EmptyParams::default()))
        .await
        .expect("the Server handler completes typed reverse calls");
    assert_eq!(reverse[0].text, "client: first");
    assert_eq!(reverse[1].text, "client: second");
    assert_eq!(client_completion_rx.recv().await.as_deref(), Some("second"));
    assert_eq!(client_completion_rx.recv().await.as_deref(), Some("first"));

    let cancelled_request = tokio::spawn({
        let server = server.clone();
        async move {
            server
                .request::<WaitForCancellation>(EmptyParams::default())
                .await
        }
    });
    within(cancellation_entered_rx.recv()).await.unwrap();
    cancelled_request.abort();
    assert!(cancelled_request.await.unwrap_err().is_cancelled());
    within(cancellation_observed_rx.recv()).await.unwrap();
    wait_for_cancellation_count(&server).await;

    within(server.shutdown())
        .await
        .expect("the public Client sends shutdown");
    server.exit().expect("the public Client sends exit");
    assert_eq!(
        within(client_serving).await.unwrap().unwrap(),
        Outcome::Exit { code: 0 }
    );
    assert_eq!(
        within(server_serving).await.unwrap().unwrap(),
        Outcome::Exit { code: 0 }
    );
}

async fn transport_failure_resolves_pending_future() {
    let (server_completions, _server_completion_rx) = mpsc::unbounded_channel();
    let (client_completions, _client_completion_rx) = mpsc::unbounded_channel();
    let (cancellation_entered, _cancellation_entered_rx) = mpsc::unbounded_channel();
    let (cancellation_observed, _cancellation_observed_rx) = mpsc::unbounded_channel();
    let (pending_entered, mut pending_entered_rx) = mpsc::unbounded_channel();
    let server = conformance_server(
        server_completions,
        cancellation_entered,
        cancellation_observed,
        pending_entered,
    );
    let (server, server_serving, client_serving, fail_client_reader) =
        connect_loopback(loopback_pair(), server, client_completions).await;

    let pending = tokio::spawn({
        let server = server.clone();
        async move { server.request::<NeverRespond>(EmptyParams::default()).await }
    });
    within(pending_entered_rx.recv()).await.unwrap();
    fail_client_reader
        .send(Err(TransportError::Malformed(
            "injected public Transport failure".into(),
        )))
        .unwrap();

    assert!(matches!(
        within(pending).await.unwrap(),
        Err(ClientError::Cancelled)
    ));
    assert!(within(client_serving).await.unwrap().is_err());
    server.disconnect();
    let _ = within(server_serving).await;
}

fn child_command() -> tokio::process::Command {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command.env(CHILD_MODE, "1");
    command
}

async fn stdio_child_journey() {
    let (client_completions, mut client_completion_rx) = mpsc::unbounded_channel();
    let child = Client::builder(ClientCapabilities::default())
        .request::<ClientEcho, _, _>(move |_ctx, params, _cancellation| {
            let completions = client_completions.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(params.delay_ms)).await;
                completions.send(params.text.clone()).unwrap();
                Ok(EchoResult {
                    text: format!("client: {}", params.text),
                })
            }
        })
        .spawn(child_command())
        .await
        .expect("the downstream Client initializes a real stdio child");
    let server = child.server();

    let first = server.request::<ServerEcho>(EchoParams {
        text: "first".into(),
        delay_ms: 60,
    });
    let second = server.request::<ServerEcho>(EchoParams {
        text: "second".into(),
        delay_ms: 5,
    });
    let (first, second) = within(async { tokio::join!(first, second) }).await;
    assert_eq!(first.unwrap().text, "server: first");
    assert_eq!(second.unwrap().text, "server: second");
    assert_eq!(
        within(server.request::<ServerEcho>(EchoParams {
            text: "third".into(),
            delay_ms: 0,
        }))
        .await
        .expect("a later direct request completes")
        .text,
        "server: third"
    );

    let reverse = tokio::spawn({
        let server = server.clone();
        async move { server.request::<ReversePair>(EmptyParams::default()).await }
    });
    assert_eq!(
        within(client_completion_rx.recv()).await.as_deref(),
        Some("second")
    );
    assert_eq!(
        within(client_completion_rx.recv()).await.as_deref(),
        Some("first")
    );
    let reverse = within(reverse)
        .await
        .unwrap()
        .expect("typed reverse calls cross the real stdio child boundary");
    assert_eq!(reverse[0].text, "client: first");
    assert_eq!(reverse[1].text, "client: second");

    let output = within(child.shutdown())
        .await
        .expect("the real stdio child closes and is reclaimed");
    assert_eq!(output.outcome(), Outcome::Exit { code: 0 });
    assert!(output.status().success());
}

async fn early_child_exit_resolves_pending_future() {
    let child = Client::builder(ClientCapabilities::default())
        .spawn(child_command())
        .await
        .expect("the early-exit child initializes");
    let server = child.server();
    let pending = tokio::spawn(async move {
        server
            .request::<ExitWithoutResponse>(EmptyParams::default())
            .await
    });

    let output = within(child.wait())
        .await
        .expect("the early-exit child reaches terminal evidence");
    assert_eq!(output.outcome(), Outcome::TransportClosed);
    assert_eq!(output.status().code(), Some(23));
    assert!(matches!(
        within(pending).await.unwrap(),
        Err(ClientError::Cancelled)
    ));
}

async fn run_stdio_server_child() {
    let (server_completions, _server_completion_rx) = mpsc::unbounded_channel();
    let (cancellation_entered, _cancellation_entered_rx) = mpsc::unbounded_channel();
    let (cancellation_observed, _cancellation_observed_rx) = mpsc::unbounded_channel();
    let (pending_entered, _pending_entered_rx) = mpsc::unbounded_channel();
    let server = conformance_server(
        server_completions,
        cancellation_entered,
        cancellation_observed,
        pending_entered,
    );
    let outcome = lspf::stdio(server)
        .serve()
        .await
        .expect("the child Server completes its public stdio journey");
    assert_eq!(outcome, Outcome::Exit { code: 0 });
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    if std::env::var_os(CHILD_MODE).is_some() {
        runtime.block_on(run_stdio_server_child());
        return;
    }
    runtime.block_on(async {
        symmetric_loopback_journey().await;
        transport_failure_resolves_pending_future().await;
        stdio_child_journey().await;
        early_child_exit_resolves_pending_future().await;
    });
}
