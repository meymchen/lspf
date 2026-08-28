use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use lspf::testing::{MemoryTransport, ScriptedPeer};
use lspf::{Outcome, RawMessage, RequestId, Server};
use serde::Serialize;
use serde_json::{Value, json};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::SoakResult;
use super::model::{Scenario, ScenarioMeasurement, ScenarioResult};

#[derive(Clone, Default)]
pub struct ResourceCounts {
    pub inbound_requests: Arc<AtomicUsize>,
    pub pending_requests: Arc<AtomicUsize>,
    pub handler_tasks: Arc<AtomicUsize>,
    pub documents: Arc<AtomicUsize>,
    pub progress_entries: Arc<AtomicUsize>,
    pub connections: Arc<AtomicUsize>,
    pub outbound_messages: Arc<AtomicUsize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resources {
    inbound_requests: usize,
    pending_requests: usize,
    handler_tasks: usize,
    documents: usize,
    progress_entries: usize,
    connections: usize,
    outbound_messages: usize,
}

impl ResourceCounts {
    pub fn snapshot(&self) -> Resources {
        Resources {
            inbound_requests: self.inbound_requests.load(Ordering::Acquire),
            pending_requests: self.pending_requests.load(Ordering::Acquire),
            handler_tasks: self.handler_tasks.load(Ordering::Acquire),
            documents: self.documents.load(Ordering::Acquire),
            progress_entries: self.progress_entries.load(Ordering::Acquire),
            connections: self.connections.load(Ordering::Acquire),
            outbound_messages: self.outbound_messages.load(Ordering::Acquire),
        }
    }

    fn assert_empty(&self, scenario: Scenario) -> SoakResult<()> {
        let resources = self.snapshot();
        if resources.inbound_requests != 0
            || resources.pending_requests != 0
            || resources.handler_tasks != 0
            || resources.documents != 0
            || resources.progress_entries != 0
            || resources.connections != 0
            || resources.outbound_messages != 0
        {
            return Err(format!("{scenario} retained resources at terminal outcome").into());
        }
        Ok(())
    }

    pub fn install_tracing(self: &Arc<Self>) -> SoakResult<()> {
        tracing::subscriber::set_global_default(Registry::default().with(ResourceLayer {
            counts: Arc::clone(self),
        }))?;
        Ok(())
    }
}

struct ResourceLayer {
    counts: Arc<ResourceCounts>,
}

#[derive(Default)]
struct ResourceEventVisitor {
    resource: Option<String>,
    current: Option<u64>,
}

impl Visit for ResourceEventVisitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "resource_current" {
            self.current = Some(value);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "resource" {
            self.resource = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        match field.name() {
            "resource" => self.resource = Some(rendered.trim_matches('"').to_owned()),
            "resource_current" => self.current = rendered.parse().ok(),
            _ => {}
        }
    }
}

impl<S> Layer<S> for ResourceLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut visitor = ResourceEventVisitor::default();
        event.record(&mut visitor);
        let Some(current) = visitor
            .current
            .and_then(|value| usize::try_from(value).ok())
        else {
            return;
        };
        let count = match visitor.resource.as_deref() {
            Some("inbound_requests") => &self.counts.inbound_requests,
            Some("pending_requests") => &self.counts.pending_requests,
            Some("documents") => &self.counts.documents,
            Some("outbound_queue") => &self.counts.outbound_messages,
            _ => return,
        };
        count.store(current, Ordering::Release);
    }
}

pub struct CountGuard<'a>(&'a AtomicUsize);

impl<'a> CountGuard<'a> {
    pub fn enter(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::AcqRel);
        Self(count)
    }
}

impl Drop for CountGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Sample {
    scenario: Scenario,
    elapsed_milliseconds: u64,
    #[serde(rename = "rssMiB")]
    rss_mib: f64,
    resources: Resources,
}

pub struct Recorder {
    writer: BufWriter<File>,
    run_started: Instant,
    interval: Duration,
    last_sample: Option<Instant>,
    pub peak_rss_mib: f64,
}

impl Recorder {
    pub fn new(path: std::path::PathBuf, interval: Duration) -> SoakResult<Self> {
        Ok(Self {
            writer: BufWriter::new(File::create(path)?),
            run_started: Instant::now(),
            interval,
            last_sample: None,
            peak_rss_mib: 0.0,
        })
    }

    fn record(
        &mut self,
        scenario: Scenario,
        resources: &ResourceCounts,
        force: bool,
    ) -> SoakResult<Option<f64>> {
        let now = Instant::now();
        if !force
            && self
                .last_sample
                .is_some_and(|last| now.duration_since(last) < self.interval)
        {
            return Ok(None);
        }
        let rss_mib = current_rss_mib()?;
        self.peak_rss_mib = self.peak_rss_mib.max(rss_mib);
        serde_json::to_writer(
            &mut self.writer,
            &Sample {
                scenario,
                elapsed_milliseconds: u64::try_from(self.run_started.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
                rss_mib,
                resources: resources.snapshot(),
            },
        )?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        self.last_sample = Some(now);
        Ok(Some(rss_mib))
    }
}

pub struct JourneyContext<'a> {
    scenario: Scenario,
    duration: Duration,
    counts: &'a Arc<ResourceCounts>,
    recorder: &'a mut Recorder,
    started: Instant,
    first_rss: f64,
}

impl<'a> JourneyContext<'a> {
    pub fn start(
        scenario: Scenario,
        duration: Duration,
        counts: &'a Arc<ResourceCounts>,
        recorder: &'a mut Recorder,
    ) -> SoakResult<Self> {
        let started = Instant::now();
        let first_rss = recorder
            .record(scenario, counts, true)?
            .expect("forced sample returns memory usage");
        Ok(Self {
            scenario,
            duration,
            counts,
            recorder,
            started,
            first_rss,
        })
    }

    pub fn counts(&self) -> &Arc<ResourceCounts> {
        self.counts
    }

    pub fn is_running(&self) -> bool {
        self.started.elapsed() < self.duration
    }

    pub fn sample(&mut self) -> SoakResult<()> {
        self.recorder.record(self.scenario, self.counts, false)?;
        Ok(())
    }

    pub fn sample_now(&mut self) -> SoakResult<()> {
        // Load-bearing resource states can be shorter than the periodic interval.
        self.recorder.record(self.scenario, self.counts, true)?;
        Ok(())
    }

    pub fn finish(
        self,
        terminal_outcome: &'static str,
        operations: u64,
        bytes: u64,
    ) -> SoakResult<ScenarioMeasurement> {
        self.counts.assert_empty(self.scenario)?;
        let final_rss = self
            .recorder
            .record(self.scenario, self.counts, true)?
            .expect("forced sample returns memory usage");
        Ok(ScenarioMeasurement {
            result: ScenarioResult {
                name: self.scenario,
                result: "success",
                terminal_outcome,
                duration_milliseconds: u64::try_from(self.started.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
                operations,
                bytes,
                terminal_resources: self.counts.snapshot(),
            },
            memory_growth_mib: (final_rss - self.first_rss).max(0.0),
        })
    }
}

pub struct ActiveConnection {
    pub peer: ScriptedPeer,
    serving: tokio::task::JoinHandle<lspf::Result<Outcome>>,
    counts: Arc<ResourceCounts>,
}

impl ActiveConnection {
    pub async fn start<S>(server: Server<S>, counts: Arc<ResourceCounts>) -> SoakResult<Self>
    where
        S: Send + Sync + 'static,
    {
        let (transport, mut peer) = MemoryTransport::pair_uncaptured();
        counts.connections.fetch_add(1, Ordering::AcqRel);
        let serving = tokio::spawn(server.serve(transport));
        peer.send(request(
            1,
            "initialize",
            &json!({"processId":null,"rootUri":null,"capabilities":{}}),
        )?)?;
        expect_success(&mut peer).await?;
        peer.send(notification("initialized", &json!({}))?)?;
        Ok(Self {
            peer,
            serving,
            counts,
        })
    }

    pub async fn finish(mut self) -> SoakResult<Outcome> {
        self.peer.send(request(2, "shutdown", &Value::Null)?)?;
        expect_success(&mut self.peer).await?;
        self.peer.send(notification("exit", &Value::Null)?)?;
        let outcome = tokio::time::timeout(Duration::from_secs(5), self.serving).await???;
        self.counts.connections.fetch_sub(1, Ordering::AcqRel);
        if outcome != (Outcome::Exit { code: 0 }) {
            return Err(format!("graceful lifecycle ended with {outcome:?}").into());
        }
        Ok(outcome)
    }

    pub async fn disconnect(self) -> SoakResult<Outcome> {
        let Self {
            peer,
            serving,
            counts,
        } = self;
        drop(peer);
        let outcome = tokio::time::timeout(Duration::from_secs(5), serving).await???;
        counts.connections.fetch_sub(1, Ordering::AcqRel);
        Ok(outcome)
    }
}

pub async fn wait_for_nonzero(count: &AtomicUsize) -> SoakResult<()> {
    wait_for_at_least(count, 1).await
}

pub async fn wait_for_at_least(count: &AtomicUsize, expected: usize) -> SoakResult<()> {
    tokio::time::timeout(Duration::from_secs(1), async {
        while count.load(Ordering::Acquire) < expected {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

pub async fn expect_success(peer: &mut ScriptedPeer) -> SoakResult<Bytes> {
    match tokio::time::timeout(Duration::from_secs(5), peer.recv()).await?? {
        RawMessage::Response {
            result: Ok(result), ..
        } => Ok(result),
        other => Err(format!("expected successful response, received {other:?}").into()),
    }
}

pub async fn expect_error(peer: &mut ScriptedPeer) -> SoakResult<()> {
    match tokio::time::timeout(Duration::from_secs(5), peer.recv()).await?? {
        RawMessage::Response { result: Err(_), .. } => Ok(()),
        other => Err(format!("expected error response, received {other:?}").into()),
    }
}

pub async fn expect_channel_success(
    output: &mut tokio::sync::mpsc::UnboundedReceiver<RawMessage>,
) -> SoakResult<Bytes> {
    match tokio::time::timeout(Duration::from_secs(5), output.recv())
        .await?
        .ok_or("server output closed early")?
    {
        RawMessage::Response {
            result: Ok(result), ..
        } => Ok(result),
        other => Err(format!("expected successful response, received {other:?}").into()),
    }
}

pub fn request(id: i32, method: &'static str, params: &impl Serialize) -> SoakResult<RawMessage> {
    Ok(RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(params)?),
    })
}

pub fn response(id: RequestId, result: &impl Serialize) -> SoakResult<RawMessage> {
    Ok(RawMessage::Response {
        id,
        result: Ok(Bytes::from(serde_json::to_vec(result)?)),
    })
}

pub fn notification(method: &'static str, params: &impl Serialize) -> SoakResult<RawMessage> {
    Ok(RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(params)?),
    })
}

pub fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Exit { .. } => "exit",
        Outcome::TransportClosed => "transport_closed",
        Outcome::WriterFailed => "writer_failed",
        Outcome::InitializeFailed => "initialize_failed",
    }
}

fn current_rss_mib() -> SoakResult<f64> {
    let status = fs::read_to_string("/proc/self/status")?;
    let kibibytes: f64 = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .ok_or("/proc/self/status did not report VmRSS")?
        .parse()?;
    Ok(kibibytes / 1024.0)
}
