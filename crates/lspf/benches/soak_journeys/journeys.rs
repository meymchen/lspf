use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lspf::types::Uri;
use lspf::types::notification::{Notification, WorkDoneProgressCancel};
use lspf::types::request::Request;
use lspf::{Outcome, RawMessage, Server};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::SoakResult;
use super::harness::{
    ActiveConnection, JourneyContext, Recorder, ResourceCounts, expect_channel_success,
    expect_error, expect_success, notification, outcome_name, request, response, wait_for_at_least,
    wait_for_nonzero,
};
use super::model::{Scenario, ScenarioMeasurement, WorkloadManifest};
use super::protocol::{
    DocumentProbe, DocumentProbeParams, Echo, EchoParams, Flood, FloodParams, ProgressRequest,
    ProgressState, RequestState, SlowPeerState, SlowTransport, Stall, document_probe, echo, flood,
    progress, progress_cancel_hook, stall,
};

pub async fn run(
    scenario: Scenario,
    workload: &WorkloadManifest,
    counts: &Arc<ResourceCounts>,
    recorder: &mut Recorder,
) -> SoakResult<ScenarioMeasurement> {
    let context = JourneyContext::start(scenario, workload.duration(), counts, recorder)?;
    match scenario {
        Scenario::Request => request_journey(workload, context).await,
        Scenario::Cancellation => cancellation_journey(workload, context).await,
        Scenario::Edit => edit_journey(workload, context).await,
        Scenario::Progress => progress_journey(workload, context).await,
        Scenario::SlowPeer => slow_peer_journey(workload, context).await,
        Scenario::Reconnect => reconnect_journey(workload, context).await,
        Scenario::Shutdown => shutdown_journey(workload, context).await,
    }
}

async fn request_journey(
    workload: &WorkloadManifest,
    mut context: JourneyContext<'_>,
) -> SoakResult<ScenarioMeasurement> {
    let state = RequestState {
        counts: context.counts().as_ref().clone(),
        release: Arc::default(),
    };
    let release = Arc::clone(&state.release);
    let server = Server::builder(state)
        .resource_policy(workload.limits.policy())
        .request::<Echo, _, _>(echo)
        .build()?;
    let mut connection = ActiveConnection::start(server, Arc::clone(context.counts())).await?;
    let payload = "request".repeat(16);
    for offset in 0..workload.traffic.request_concurrency {
        connection.peer.send(request(
            1000 + i32::try_from(offset)?,
            Echo::METHOD,
            &EchoParams {
                payload: payload.clone(),
            },
        )?)?;
    }
    wait_for_at_least(
        &context.counts().handler_tasks,
        workload.traffic.request_concurrency,
    )
    .await?;
    while context.is_running() {
        context.sample()?;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    release.notify_waiters();
    for _ in 0..workload.traffic.request_concurrency {
        expect_success(&mut connection.peer).await?;
    }
    let operations = u64::try_from(workload.traffic.request_concurrency)?;
    let outcome = connection.finish().await?;
    context.finish(
        outcome_name(outcome),
        operations,
        operations * u64::try_from(payload.len())?,
    )
}

async fn cancellation_journey(
    workload: &WorkloadManifest,
    mut context: JourneyContext<'_>,
) -> SoakResult<ScenarioMeasurement> {
    let server = Server::builder(context.counts().as_ref().clone())
        .resource_policy(workload.limits.policy())
        .request::<Stall, _, _>(stall)
        .build()?;
    let mut connection = ActiveConnection::start(server, Arc::clone(context.counts())).await?;
    let mut operations = 0_u64;
    while context.is_running() {
        let mut ids = Vec::with_capacity(workload.traffic.cancellation_concurrency);
        for offset in 0..workload.traffic.cancellation_concurrency {
            let id = 1000 + i32::try_from(operations)? + i32::try_from(offset)?;
            ids.push(id);
            connection.peer.send(request(id, Stall::METHOD, &())?)?;
        }
        tokio::task::yield_now().await;
        wait_for_nonzero(&context.counts().handler_tasks).await?;
        context.sample()?;
        for id in &ids {
            connection
                .peer
                .send(notification("$/cancelRequest", &json!({"id": id}))?)?;
        }
        for _ in ids {
            expect_error(&mut connection.peer).await?;
        }
        operations += u64::try_from(workload.traffic.cancellation_concurrency)?;
        context.sample()?;
    }
    let outcome = connection.finish().await?;
    context.finish(outcome_name(outcome), operations, 0)
}

async fn edit_journey(
    workload: &WorkloadManifest,
    mut context: JourneyContext<'_>,
) -> SoakResult<ScenarioMeasurement> {
    let server = Server::builder(context.counts().as_ref().clone())
        .resource_policy(workload.limits.policy())
        .request::<DocumentProbe, _, _>(document_probe)
        .build()?;
    let mut connection = ActiveConnection::start(server, Arc::clone(context.counts())).await?;
    let uri = Uri::from_str("file:///soak.txt")?;
    let document = "a".repeat(workload.traffic.edit_document_bytes);
    connection.peer.send(notification(
        "textDocument/didOpen",
        &json!({"textDocument":{"uri":uri.as_str(),"languageId":"text","version":1,"text":document}}),
    )?)?;
    let mut operations = 0_u64;
    let mut version = 2_i32;
    while context.is_running() {
        connection.peer.send(notification(
            "textDocument/didChange",
            &json!({"textDocument":{"uri":uri.as_str(),"version":version},"contentChanges":[{"text":document}]}),
        )?)?;
        connection.peer.send(request(
            1000 + version,
            DocumentProbe::METHOD,
            &DocumentProbeParams { uri: uri.clone() },
        )?)?;
        let result = expect_success(&mut connection.peer).await?;
        if serde_json::from_slice::<Option<i32>>(&result)? != Some(version) {
            return Err("edit probe did not observe the latest version".into());
        }
        version += 1;
        operations += 1;
        context.sample()?;
    }
    connection.peer.send(notification(
        "textDocument/didClose",
        &json!({"textDocument":{"uri":uri.as_str()}}),
    )?)?;
    connection.peer.send(request(
        999_999,
        DocumentProbe::METHOD,
        &DocumentProbeParams { uri },
    )?)?;
    if serde_json::from_slice::<Option<i32>>(&expect_success(&mut connection.peer).await?)?
        .is_some()
    {
        return Err("closed document remained retained".into());
    }
    let outcome = connection.finish().await?;
    context.finish(
        outcome_name(outcome),
        operations,
        operations * u64::try_from(workload.traffic.edit_document_bytes)?,
    )
}

async fn progress_journey(
    workload: &WorkloadManifest,
    mut context: JourneyContext<'_>,
) -> SoakResult<ScenarioMeasurement> {
    let state = ProgressState {
        counts: context.counts().as_ref().clone(),
        ended: Arc::default(),
        retained_entry: Arc::default(),
        hooks_seen: Arc::default(),
        hook_notify: Arc::default(),
    };
    let observation = state.clone();
    let server = Server::builder(state)
        .resource_policy(workload.limits.policy())
        .request::<ProgressRequest, _, _>(progress)
        .notification::<WorkDoneProgressCancel, _, _>(progress_cancel_hook)
        .build()?;
    let mut connection = ActiveConnection::start(server, Arc::clone(context.counts())).await?;
    let mut operations = 0_u64;
    while context.is_running() {
        let batch = workload.traffic.progress_concurrency;
        for offset in 0..batch {
            let id = 1000 + i32::try_from(operations)? + i32::try_from(offset)?;
            connection
                .peer
                .send(request(id, ProgressRequest::METHOD, &())?)?;
        }

        let mut tokens = Vec::with_capacity(batch);
        let mut completed = 0;
        let mut notifications = 0;
        while completed < batch || notifications < batch * 3 {
            match tokio::time::timeout(Duration::from_secs(5), connection.peer.recv()).await?? {
                RawMessage::Request { id, method, params }
                    if method.as_ref() == "window/workDoneProgress/create" =>
                {
                    let params: Value = serde_json::from_slice(&params)?;
                    tokens.push(params["token"].clone());
                    connection.peer.send(response(id, &Value::Null)?)?;
                }
                RawMessage::Notification { method, .. } if method.as_ref() == "$/progress" => {
                    notifications += 1;
                    if notifications <= batch * 2 {
                        context.sample()?;
                    }
                }
                RawMessage::Response { result: Ok(_), .. } => completed += 1,
                other => return Err(format!("unexpected progress traffic: {other:?}").into()),
            }
        }
        if tokens.len() != batch {
            return Err("progress batch did not create one token per request".into());
        }
        for token in tokens {
            connection.peer.send(notification(
                WorkDoneProgressCancel::METHOD,
                &json!({"token": token}),
            )?)?;
        }
        let expected_hooks = usize::try_from(operations)? + batch;
        tokio::time::timeout(Duration::from_secs(5), async {
            while observation.hooks_seen.load(Ordering::Acquire) < expected_hooks {
                observation.hook_notify.notified().await;
            }
        })
        .await?;
        if observation.retained_entry.swap(false, Ordering::AcqRel) {
            return Err("ended progress token remained in the connection registry".into());
        }
        operations += u64::try_from(batch)?;
    }
    let outcome = connection.finish().await?;
    context.finish(outcome_name(outcome), operations, 0)
}

async fn slow_peer_journey(
    workload: &WorkloadManifest,
    mut context: JourneyContext<'_>,
) -> SoakResult<ScenarioMeasurement> {
    let mut operations = 0_u64;
    let mut bytes = 0_u64;
    while context.is_running() {
        let (input, incoming) = mpsc::unbounded_channel();
        let (outgoing, mut output) = mpsc::unbounded_channel();
        let (completed, completion) = oneshot::channel();
        let writes_blocked = Arc::new(AtomicBool::new(false));
        let write_release = Arc::new(tokio::sync::Semaphore::new(0));
        let server = Server::builder(SlowPeerState {
            counts: Arc::clone(context.counts()),
            completed: Mutex::new(Some(completed)),
        })
        .resource_policy(workload.limits.policy())
        .notification::<Flood, _, _>(flood)
        .build()?;
        context.counts().connections.fetch_add(1, Ordering::AcqRel);
        let serving = tokio::spawn(server.serve(SlowTransport {
            incoming,
            outgoing,
            delay: Duration::from_millis(2),
            writes_blocked: Arc::clone(&writes_blocked),
            write_release: Arc::clone(&write_release),
        }));
        input.send(request(
            1,
            "initialize",
            &json!({"processId":null,"rootUri":null,"capabilities":{}}),
        )?)?;
        expect_channel_success(&mut output).await?;
        input.send(notification("initialized", &json!({}))?)?;
        // Hold admitted flood messages until their non-zero queue depth is sampled.
        writes_blocked.store(true, Ordering::Release);
        let release_writes = || {
            writes_blocked.store(false, Ordering::Release);
            write_release.add_permits(1);
        };
        input.send(notification(
            Flood::METHOD,
            &FloodParams {
                attempts: workload.traffic.slow_peer_attempts_per_cycle,
            },
        )?)?;
        let (accepted, overloaded) = match completion.await {
            Ok(result) => result,
            Err(error) => {
                release_writes();
                return Err(error.into());
            }
        };
        if overloaded == 0 {
            release_writes();
            return Err("slow peer did not exercise outbound overload".into());
        }
        let sample = context.sample_now();
        release_writes();
        sample?;
        for _ in 0..accepted {
            output
                .recv()
                .await
                .ok_or("slow peer closed before delivery")?;
        }
        operations += u64::try_from(accepted + overloaded)?;
        bytes += u64::try_from(accepted)? * 1024;
        drop(input);
        let outcome = tokio::time::timeout(Duration::from_secs(5), serving).await???;
        if outcome != Outcome::TransportClosed {
            return Err(format!("slow-peer outcome was {outcome:?}").into());
        }
        context.counts().connections.fetch_sub(1, Ordering::AcqRel);
        context.sample()?;
    }
    context.finish("transport_closed", operations, bytes)
}

async fn reconnect_journey(
    workload: &WorkloadManifest,
    mut context: JourneyContext<'_>,
) -> SoakResult<ScenarioMeasurement> {
    let mut operations = 0_u64;
    while context.is_running() {
        for _ in 0..workload.traffic.reconnects_per_cycle {
            let server = Server::builder(context.counts().as_ref().clone())
                .resource_policy(workload.limits.policy())
                .build()?;
            let connection = ActiveConnection::start(server, Arc::clone(context.counts())).await?;
            if connection.disconnect().await? != Outcome::TransportClosed {
                return Err("reconnect did not end in transport closure".into());
            }
            operations += 1;
        }
        context.sample()?;
    }
    context.finish("transport_closed", operations, 0)
}

async fn shutdown_journey(
    workload: &WorkloadManifest,
    mut context: JourneyContext<'_>,
) -> SoakResult<ScenarioMeasurement> {
    let mut operations = 0_u64;
    while context.is_running() {
        for _ in 0..workload.traffic.shutdowns_per_cycle {
            let server = Server::builder(context.counts().as_ref().clone())
                .resource_policy(workload.limits.policy())
                .build()?;
            let connection = ActiveConnection::start(server, Arc::clone(context.counts())).await?;
            if connection.finish().await? != (Outcome::Exit { code: 0 }) {
                return Err("shutdown did not end in exit".into());
            }
            operations += 1;
        }
        context.sample()?;
    }
    context.finish("exit", operations, 0)
}
