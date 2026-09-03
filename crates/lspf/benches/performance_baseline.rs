use std::borrow::Cow;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use lspf::testing::{ScriptedPeer, ServerJourney};
use lspf::types::notification::Notification;
use lspf::types::request::Request;
use lspf::types::{
    DocumentSymbolOptions, DocumentSymbolParams, DocumentSymbolPartialResponse,
    DocumentSymbolRequest, DocumentSymbolResponse, Uri,
};
use lspf::{
    CancellationToken, ClientError, LspError, RawMessage, RequestId, ResourcePolicy, Server,
    ServerContext, Transport, TransportError, TransportReader, TransportWriter,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

type BenchmarkResult<T> = Result<T, Box<dyn Error>>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkloadManifest {
    schema_version: u64,
    workload_version: u64,
    workloads: Workloads,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Workloads {
    startup: StartupWorkload,
    request_latency: RequestLatencyWorkload,
    throughput: ThroughputWorkload,
    large_document_editing: LargeDocumentWorkload,
    notebook_editing: NotebookEditingWorkload,
    partial_result_throughput: PartialResultThroughputWorkload,
    slow_peer: SlowPeerWorkload,
}

#[derive(Deserialize)]
struct StartupWorkload {
    iterations: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestLatencyWorkload {
    warmup_operations: usize,
    measured_operations: usize,
}

#[derive(Deserialize)]
struct ThroughputWorkload {
    operations: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LargeDocumentWorkload {
    document_bytes: usize,
    edits: usize,
    replacement_bytes: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotebookEditingWorkload {
    cells: usize,
    edits: usize,
    replacement_bytes: usize,
}

#[derive(Deserialize)]
struct PartialResultThroughputWorkload {
    chunks: usize,
}

struct NotebookEditingMeasurement {
    open: Duration,
    edits: Vec<Duration>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SlowPeerWorkload {
    attempts: usize,
    outbound_message_limit: usize,
    outbound_byte_limit: usize,
    write_delay_ms: u64,
}

struct Arguments {
    workloads: PathBuf,
    output: PathBuf,
    revision: String,
}

enum Ping {}

impl Request for Ping {
    type Params = u64;
    type Result = u64;
    const METHOD: &'static str = "performance/ping";
}

async fn ping(
    _state: Arc<()>,
    _context: ServerContext,
    value: u64,
    _cancellation: CancellationToken,
) -> Result<u64, LspError> {
    Ok(value)
}

#[derive(Deserialize, Serialize)]
struct DocumentProbeParams {
    uri: Uri,
}

enum DocumentProbe {}

impl Request for DocumentProbe {
    type Params = DocumentProbeParams;
    type Result = Option<i32>;
    const METHOD: &'static str = "performance/documentVersion";
}

async fn document_probe(
    _state: Arc<()>,
    context: ServerContext,
    params: DocumentProbeParams,
    _cancellation: CancellationToken,
) -> Result<Option<i32>, LspError> {
    Ok(context
        .documents()
        .get(&params.uri)
        .and_then(|document| document.version()))
}

struct PartialResultState {
    chunks: usize,
}

async fn stream_partial_results(
    state: Arc<PartialResultState>,
    context: ServerContext,
    _params: DocumentSymbolParams,
    _cancellation: CancellationToken,
) -> Result<Option<DocumentSymbolResponse>, LspError> {
    let sink = context
        .partial_results::<DocumentSymbolRequest>()
        .ok_or_else(|| LspError::internal("partial-result token was not available"))?;
    for _ in 0..state.chunks {
        sink.report(DocumentSymbolPartialResponse::DocumentSymbolList(Vec::new()))
            .map_err(LspError::internal)?;
    }
    Ok(None)
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct FloodParams {
    attempts: usize,
}

enum Flood {}

impl Notification for Flood {
    type Params = FloodParams;
    const METHOD: &'static str = "performance/flood";
}

enum SlowMessage {}

impl Notification for SlowMessage {
    type Params = ();
    const METHOD: &'static str = "performance/slowMessage";
}

#[derive(Clone, Copy, Debug)]
struct FloodResult {
    attempted: usize,
    accepted: usize,
    overloaded: usize,
}

struct SlowPeerState {
    completed: Mutex<Option<oneshot::Sender<FloodResult>>>,
}

async fn flood(state: Arc<SlowPeerState>, context: ServerContext, params: FloodParams) {
    let mut accepted = 0;
    let mut overloaded = 0;
    for _ in 0..params.attempts {
        match context.client().notify::<SlowMessage>(()) {
            Ok(()) => accepted += 1,
            Err(ClientError::OutboundOverloaded) => overloaded += 1,
            Err(error) => panic!("unexpected slow-peer notification failure: {error}"),
        }
    }
    let result = FloodResult {
        attempted: params.attempts,
        accepted,
        overloaded,
    };
    state
        .completed
        .lock()
        .unwrap()
        .take()
        .expect("slow-peer workload runs once")
        .send(result)
        .ok();
}

struct SlowTransport {
    incoming: mpsc::UnboundedReceiver<RawMessage>,
    outgoing: mpsc::UnboundedSender<RawMessage>,
    write_delay: Duration,
}

struct SlowReader(mpsc::UnboundedReceiver<RawMessage>);

struct SlowWriter {
    outgoing: mpsc::UnboundedSender<RawMessage>,
    write_delay: Duration,
}

impl Transport for SlowTransport {
    type Reader = SlowReader;
    type Writer = SlowWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            SlowReader(self.incoming),
            SlowWriter {
                outgoing: self.outgoing,
                write_delay: self.write_delay,
            },
        )
    }
}

impl TransportReader for SlowReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.0.recv().await.ok_or(TransportError::Closed)
    }
}

impl TransportWriter for SlowWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        tokio::time::sleep(self.write_delay).await;
        self.outgoing
            .send(message)
            .map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> BenchmarkResult<()> {
    let raw_arguments: Vec<_> = std::env::args().skip(1).collect();
    if raw_arguments.is_empty()
        || raw_arguments
            .iter()
            .any(|argument| argument.starts_with("--test-threads"))
    {
        return Ok(());
    }
    let arguments = parse_arguments()?;
    let manifest: WorkloadManifest = serde_json::from_slice(&fs::read(&arguments.workloads)?)?;
    validate_manifest(&manifest)?;

    let startup = measure_startup(&manifest.workloads.startup).await?;
    let request_latency = measure_request_latency(&manifest.workloads.request_latency).await?;
    let throughput = measure_throughput(&manifest.workloads.throughput).await?;
    let large_document =
        measure_large_document_editing(&manifest.workloads.large_document_editing).await?;
    let notebook_editing = measure_notebook_editing(&manifest.workloads.notebook_editing).await?;
    let partial_result_throughput =
        measure_partial_result_throughput(&manifest.workloads.partial_result_throughput).await?;
    let slow_peer = measure_slow_peer(manifest.workloads.slow_peer).await?;
    let peak_rss_mib = peak_rss_mib()?;

    let results = json!({
        "schemaVersion": 1,
        "workloadVersion": manifest.workload_version,
        "environmentMetadataVersion": 1,
        "revision": arguments.revision,
        "environment": {
            "os": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "logicalCpuCount": std::thread::available_parallelism()?.get(),
            "rustc": rustc_version()?,
            "profile": "bench"
        },
        "latencyMs": {
            "startupP95": percentile_ms(&startup, 0.95),
            "requestP95": percentile_ms(&request_latency, 0.95),
            "requestP99": percentile_ms(&request_latency, 0.99),
            "largeDocumentEditP95": percentile_ms(&large_document, 0.95),
            "largeDocumentEditP99": percentile_ms(&large_document, 0.99),
            "notebookOpen": notebook_editing.open.as_secs_f64() * 1000.0,
            "notebookEditP95": percentile_ms(&notebook_editing.edits, 0.95),
            "notebookEditP99": percentile_ms(&notebook_editing.edits, 0.99)
        },
        "throughputOperationsPerSecond": throughput,
        "partialResultChunksPerSecond": partial_result_throughput,
        "peakRssMiB": peak_rss_mib,
        "limitBehavior": {
            "slowPeer": {
                "outboundMessageLimit": manifest.workloads.slow_peer.outbound_message_limit,
                "attempted": slow_peer.attempted,
                "accepted": slow_peer.accepted,
                "overloaded": slow_peer.overloaded,
                "delivered": slow_peer.accepted
            }
        }
    });
    fs::write(arguments.output, serde_json::to_vec_pretty(&results)?)?;
    Ok(())
}

fn parse_arguments() -> BenchmarkResult<Arguments> {
    let mut args = std::env::args().skip(1);
    let mut workloads = None;
    let mut output = None;
    let mut revision = None;
    while let Some(argument) = args.next() {
        if argument == "--bench" {
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {argument}"))?;
        match argument.as_str() {
            "--workloads" => workloads = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--revision" => revision = Some(value),
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    Ok(Arguments {
        workloads: workloads.ok_or("missing --workloads")?,
        output: output.ok_or("missing --output")?,
        revision: revision.ok_or("missing --revision")?,
    })
}

fn validate_manifest(manifest: &WorkloadManifest) -> BenchmarkResult<()> {
    if manifest.schema_version != 1 {
        return Err("unsupported workload schema version".into());
    }
    let workloads = &manifest.workloads;
    if workloads.startup.iterations == 0
        || workloads.request_latency.measured_operations == 0
        || workloads.throughput.operations == 0
        || workloads.large_document_editing.document_bytes == 0
        || workloads.large_document_editing.edits == 0
        || workloads.large_document_editing.replacement_bytes == 0
        || workloads.large_document_editing.replacement_bytes
            > workloads.large_document_editing.document_bytes
        || workloads.notebook_editing.cells == 0
        || workloads.notebook_editing.edits == 0
        || workloads.notebook_editing.replacement_bytes == 0
        || workloads.partial_result_throughput.chunks == 0
        || workloads.slow_peer.attempts == 0
        || workloads.slow_peer.outbound_message_limit == 0
        || workloads.slow_peer.outbound_byte_limit == 0
        || workloads.slow_peer.write_delay_ms == 0
    {
        return Err("performance workload counts and limits must be positive and valid".into());
    }
    Ok(())
}

async fn measure_notebook_editing(
    workload: &NotebookEditingWorkload,
) -> BenchmarkResult<NotebookEditingMeasurement> {
    let server = Server::builder(())
        .request::<DocumentProbe, _, _>(document_probe)
        .build()?;
    let mut journey = ServerJourney::start(server).await?;
    let notebook_uri = "file:///performance-baseline.ipynb";
    let cell_uris: Vec<_> = (0..workload.cells)
        .map(|index| format!("{notebook_uri}#cell-{index}"))
        .collect();
    let cells: Vec<_> = cell_uris
        .iter()
        .map(|uri| json!({"kind": 2, "document": uri}))
        .collect();
    let cell_documents: Vec<_> = cell_uris
        .iter()
        .map(|uri| {
            json!({
                "uri": uri,
                "languageId": "plaintext",
                "version": 1,
                "text": "a"
            })
        })
        .collect();
    let open_started = Instant::now();
    journey.peer().send(notification(
        "notebookDocument/didOpen",
        &json!({
            "notebookDocument": {
                "uri": notebook_uri,
                "notebookType": "jupyter-notebook",
                "version": 1,
                "cells": cells
            },
            "cellTextDocuments": cell_documents
        }),
    )?)?;
    let first_cell = Uri::from_str(&cell_uris[0])?;
    probe_document(journey.peer(), 30_000, &first_cell, 1).await?;
    let open = open_started.elapsed();

    let replacement = "b".repeat(workload.replacement_bytes);
    let mut samples = Vec::with_capacity(workload.edits);
    for edit in 0..workload.edits {
        let version = i32::try_from(edit + 2)?;
        let cell_uri = Uri::from_str(&cell_uris[edit % cell_uris.len()])?;
        let started = Instant::now();
        journey.peer().send(notification(
            "notebookDocument/didChange",
            &json!({
                "notebookDocument": {"uri": notebook_uri, "version": version},
                "change": {
                    "cells": {
                        "textContent": [{
                            "document": {"uri": cell_uri.as_str(), "version": version},
                            "changes": [{"text": replacement}]
                        }]
                    }
                }
            }),
        )?)?;
        probe_document(journey.peer(), 30_001 + version, &cell_uri, version).await?;
        samples.push(started.elapsed());
    }

    journey.peer().send(notification(
        "notebookDocument/didClose",
        &json!({
            "notebookDocument": {"uri": notebook_uri},
            "cellTextDocuments": []
        }),
    )?)?;
    journey.finish().await?;
    Ok(NotebookEditingMeasurement {
        open,
        edits: samples,
    })
}

async fn measure_partial_result_throughput(
    workload: &PartialResultThroughputWorkload,
) -> BenchmarkResult<f64> {
    let max_outbound_messages = workload
        .chunks
        .checked_add(2)
        .ok_or("partial-result chunk count is too large")?;
    let server = Server::builder(PartialResultState {
        chunks: workload.chunks,
    })
    .resource_policy(ResourcePolicy {
        max_outbound_messages,
        ..ResourcePolicy::default()
    })
    .feature(
        lspf::features::document_symbol(DocumentSymbolOptions::default()),
        stream_partial_results,
    )
    .build()?;
    let mut journey = ServerJourney::start(server).await?;
    let started = Instant::now();
    journey.peer().send(request(
        40_000,
        DocumentSymbolRequest::METHOD,
        &json!({
            "textDocument": {"uri": "file:///performance-baseline.rs"},
            "partialResultToken": "performance-baseline"
        }),
    )?)?;
    for _ in 0..workload.chunks {
        let message = journey.peer().recv().await?;
        if message.method() != Some("$/progress") {
            return Err(format!("unexpected partial-result message: {message:?}").into());
        }
    }
    expect_success(journey.peer()).await?;
    let elapsed = started.elapsed().as_secs_f64();
    journey.finish().await?;
    Ok(workload.chunks as f64 / elapsed)
}

async fn measure_startup(workload: &StartupWorkload) -> BenchmarkResult<Vec<Duration>> {
    let mut samples = Vec::with_capacity(workload.iterations);
    for _ in 0..workload.iterations {
        let started = Instant::now();
        let journey = ServerJourney::start(Server::builder(()).build()?).await?;
        samples.push(started.elapsed());
        journey.finish().await?;
    }
    Ok(samples)
}

fn ping_server() -> BenchmarkResult<Server<()>> {
    Ok(Server::builder(()).request::<Ping, _, _>(ping).build()?)
}

async fn measure_request_latency(
    workload: &RequestLatencyWorkload,
) -> BenchmarkResult<Vec<Duration>> {
    let mut journey = ServerJourney::start(ping_server()?).await?;
    let mut request_id = 10;
    for _ in 0..workload.warmup_operations {
        round_trip_ping(journey.peer(), request_id).await?;
        request_id += 1;
    }
    let mut samples = Vec::with_capacity(workload.measured_operations);
    for _ in 0..workload.measured_operations {
        let started = Instant::now();
        round_trip_ping(journey.peer(), request_id).await?;
        samples.push(started.elapsed());
        request_id += 1;
    }
    journey.finish().await?;
    Ok(samples)
}

async fn measure_throughput(workload: &ThroughputWorkload) -> BenchmarkResult<f64> {
    const BATCH_SIZE: usize = 32;
    let mut journey = ServerJourney::start(ping_server()?).await?;
    let started = Instant::now();
    let mut completed = 0;
    while completed < workload.operations {
        let batch = BATCH_SIZE.min(workload.operations - completed);
        for offset in 0..batch {
            let id = (completed + offset + 1000) as i32;
            journey.peer().send(request(id, Ping::METHOD, &id)?)?;
        }
        for _ in 0..batch {
            expect_success(journey.peer()).await?;
        }
        completed += batch;
    }
    let elapsed = started.elapsed().as_secs_f64();
    journey.finish().await?;
    Ok(workload.operations as f64 / elapsed)
}

async fn round_trip_ping(peer: &mut ScriptedPeer, id: i32) -> BenchmarkResult<()> {
    peer.send(request(id, Ping::METHOD, &id)?)?;
    let result = expect_success(peer).await?;
    let echoed: i32 = serde_json::from_slice(&result)?;
    if echoed != id {
        return Err("ping response did not echo its input".into());
    }
    Ok(())
}

async fn measure_large_document_editing(
    workload: &LargeDocumentWorkload,
) -> BenchmarkResult<Vec<Duration>> {
    let server = Server::builder(())
        .request::<DocumentProbe, _, _>(document_probe)
        .build()?;
    let mut journey = ServerJourney::start(server).await?;
    let uri = Uri::from_str("file:///performance-baseline.txt")?;
    let document = "a".repeat(workload.document_bytes);
    journey.peer().send(notification(
        "textDocument/didOpen",
        &json!({
            "textDocument": {
                "uri": uri.as_str(),
                "languageId": "plaintext",
                "version": 1,
                "text": document
            }
        }),
    )?)?;
    probe_document(journey.peer(), 20_000, &uri, 1).await?;

    let replacement = "b".repeat(workload.replacement_bytes);
    let mut samples = Vec::with_capacity(workload.edits);
    for edit in 0..workload.edits {
        let version = i32::try_from(edit + 2)?;
        let started = Instant::now();
        journey.peer().send(notification(
            "textDocument/didChange",
            &json!({
                "textDocument": { "uri": uri.as_str(), "version": version },
                "contentChanges": [{
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": {
                            "line": 0,
                            "character": workload.replacement_bytes
                        }
                    },
                    "text": replacement
                }]
            }),
        )?)?;
        probe_document(journey.peer(), 20_001 + version, &uri, version).await?;
        samples.push(started.elapsed());
    }

    journey.peer().send(notification(
        "textDocument/didClose",
        &json!({ "textDocument": { "uri": uri.as_str() } }),
    )?)?;
    journey.finish().await?;
    Ok(samples)
}

async fn probe_document(
    peer: &mut ScriptedPeer,
    id: i32,
    uri: &Uri,
    expected_version: i32,
) -> BenchmarkResult<()> {
    peer.send(request(
        id,
        DocumentProbe::METHOD,
        &DocumentProbeParams { uri: uri.clone() },
    )?)?;
    let result = expect_success(peer).await?;
    let version: Option<i32> = serde_json::from_slice(&result)?;
    if version != Some(expected_version) {
        return Err(format!(
            "document probe expected version {expected_version}, received {version:?}"
        )
        .into());
    }
    Ok(())
}

async fn measure_slow_peer(workload: SlowPeerWorkload) -> BenchmarkResult<FloodResult> {
    let (input, incoming) = mpsc::unbounded_channel();
    let (outgoing, mut output) = mpsc::unbounded_channel();
    let transport = SlowTransport {
        incoming,
        outgoing,
        write_delay: Duration::from_millis(workload.write_delay_ms),
    };
    let (completed, completion) = oneshot::channel();
    let server = Server::builder(SlowPeerState {
        completed: Mutex::new(Some(completed)),
    })
    .notification::<Flood, _, _>(flood)
    .resource_policy(ResourcePolicy {
        max_outbound_messages: workload.outbound_message_limit,
        max_outbound_bytes: workload.outbound_byte_limit,
        ..ResourcePolicy::default()
    })
    .build()?;
    let serving = tokio::spawn(server.serve(transport));

    input.send(request(
        1,
        "initialize",
        &json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    )?)?;
    expect_channel_success(&mut output).await?;
    input.send(notification("initialized", &json!({}))?)?;
    input.send(notification(
        Flood::METHOD,
        &FloodParams {
            attempts: workload.attempts,
        },
    )?)?;
    let result = completion.await?;
    for _ in 0..result.accepted {
        let message = output.recv().await.ok_or("slow peer closed early")?;
        if message.method() != Some(SlowMessage::METHOD) {
            return Err(format!("unexpected slow-peer message: {message:?}").into());
        }
    }
    drop(input);
    serving.await??;
    Ok(result)
}

async fn expect_channel_success(
    output: &mut mpsc::UnboundedReceiver<RawMessage>,
) -> BenchmarkResult<Bytes> {
    let message = output.recv().await.ok_or("server output closed early")?;
    successful_response(message)
}

async fn expect_success(peer: &mut ScriptedPeer) -> BenchmarkResult<Bytes> {
    successful_response(peer.recv().await?)
}

fn successful_response(message: RawMessage) -> BenchmarkResult<Bytes> {
    match message {
        RawMessage::Response {
            result: Ok(result), ..
        } => Ok(result),
        other => Err(format!("expected successful response, received {other:?}").into()),
    }
}

fn request(
    id: i32,
    method: &'static str,
    params: &impl Serialize,
) -> Result<RawMessage, serde_json::Error> {
    Ok(RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(params)?),
    })
}

fn notification(
    method: &'static str,
    params: &impl Serialize,
) -> Result<RawMessage, serde_json::Error> {
    Ok(RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(params)?),
    })
}

fn percentile_ms(samples: &[Duration], percentile: f64) -> f64 {
    let mut milliseconds: Vec<_> = samples.iter().map(Duration::as_secs_f64).collect();
    milliseconds.sort_by(f64::total_cmp);
    let index = ((milliseconds.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(milliseconds.len() - 1);
    milliseconds[index] * 1000.0
}

fn peak_rss_mib() -> BenchmarkResult<f64> {
    let status = fs::read_to_string("/proc/self/status")?;
    let kibibytes: f64 = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .ok_or("/proc/self/status did not report VmHWM")?
        .parse()?;
    Ok(kibibytes / 1024.0)
}

fn rustc_version() -> BenchmarkResult<String> {
    let output = Command::new("rustc").arg("--version").output()?;
    if !output.status.success() {
        return Err("rustc --version failed".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
