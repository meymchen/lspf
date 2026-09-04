use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lspf::types::notification::{Notification, WorkDoneProgressCancel};
use lspf::types::request::Request;
use lspf::types::{
    DocumentSymbolOptions, NotebookDocumentFilterWithNotebook, NotebookDocumentSyncOptions, Uri,
};
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
    DocumentProbe, DocumentProbeParams, Echo, EchoParams, Flood, FloodParams, PartialResultState,
    ProgressRequest, ProgressState, RequestState, SlowPeerState, SlowTransport, Stall,
    document_probe, echo, flood, partial_result_burst, progress, progress_cancel_hook, stall,
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
        Scenario::Notebook => notebook_journey(workload, context).await,
        Scenario::PartialResult => partial_result_journey(workload, context).await,
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

async fn notebook_journey(
    workload: &WorkloadManifest,
    mut context: JourneyContext<'_>,
) -> SoakResult<ScenarioMeasurement> {
    let server = Server::builder(context.counts().as_ref().clone())
        .resource_policy(workload.limits.policy())
        // Notebook built-ins are reachable only once a server opts in
        // (ADR 0034), so this journey advertises the capability it exercises.
        .notebook_document_sync(NotebookDocumentSyncOptions::new(
            vec![NotebookDocumentFilterWithNotebook::new("jupyter-notebook".into(), None).into()],
            Some(true),
        ))
        .request::<DocumentProbe, _, _>(document_probe)
        .build()?;
    let mut connection = ActiveConnection::start(server, Arc::clone(context.counts())).await?;
    let notebook_uri = "file:///soak.ipynb";
    let cell_uris: Vec<_> = (0..workload.traffic.notebook_cells)
        .map(|index| format!("{notebook_uri}#cell-{index}"))
        .collect();
    let cells: Vec<_> = cell_uris
        .iter()
        .map(|uri| json!({"kind": 2, "document": uri}))
        .collect();
    let mut request_id = 1000_i32;
    let mut operations = 0_u64;

    while context.is_running() || operations < workload.traffic.notebook_minimum_cycles {
        for _ in 0..workload.traffic.notebook_cycles_per_batch {
            let cell_documents: Vec<_> = cell_uris
                .iter()
                .map(|uri| {
                    json!({
                        "uri": uri,
                        "languageId": "text",
                        "version": 1,
                        "text": "before"
                    })
                })
                .collect();
            connection.peer.send(notification(
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

            let text_content: Vec<_> = cell_uris
                .iter()
                .map(|uri| {
                    json!({
                        "document": {"uri": uri, "version": 2},
                        "changes": [{"text": "after"}]
                    })
                })
                .collect();
            connection.peer.send(notification(
                "notebookDocument/didChange",
                &json!({
                    "notebookDocument": {"uri": notebook_uri, "version": 2},
                    "change": {"cells": {"textContent": text_content}}
                }),
            )?)?;
            let first_cell = Uri::from_str(&cell_uris[0])?;
            connection.peer.send(request(
                request_id,
                DocumentProbe::METHOD,
                &DocumentProbeParams {
                    uri: first_cell.clone(),
                },
            )?)?;
            request_id += 1;
            let result = expect_success(&mut connection.peer).await?;
            if serde_json::from_slice::<Option<i32>>(&result)? != Some(2) {
                return Err("notebook probe did not observe the mutated cell".into());
            }
            context.sample_now()?;

            connection.peer.send(notification(
                "notebookDocument/didClose",
                &json!({
                    "notebookDocument": {"uri": notebook_uri},
                    "cellTextDocuments": []
                }),
            )?)?;
            connection.peer.send(request(
                request_id,
                DocumentProbe::METHOD,
                &DocumentProbeParams { uri: first_cell },
            )?)?;
            request_id += 1;
            if serde_json::from_slice::<Option<i32>>(&expect_success(&mut connection.peer).await?)?
                .is_some()
            {
                return Err("closed notebook retained a cell document".into());
            }
            operations += 1;
            context.sample()?;
        }
    }
    let outcome = connection.finish().await?;
    context.finish(
        outcome_name(outcome),
        operations,
        operations * u64::try_from(workload.traffic.notebook_cells)? * 11,
    )
}

async fn partial_result_journey(
    workload: &WorkloadManifest,
    mut context: JourneyContext<'_>,
) -> SoakResult<ScenarioMeasurement> {
    let mut operations = 0_u64;
    let mut bytes = 0_u64;
    while context.is_running() {
        let (input, incoming) = mpsc::unbounded_channel();
        let (outgoing, mut output) = mpsc::unbounded_channel();
        let (completed, completion) = oneshot::channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let writes_blocked = Arc::new(AtomicBool::new(false));
        let write_release = Arc::new(tokio::sync::Semaphore::new(0));
        let server = Server::builder(PartialResultState {
            counts: Arc::clone(context.counts()),
            chunks: workload.traffic.partial_result_chunks_per_burst,
            completed: Mutex::new(Some(completed)),
            release: Arc::clone(&release),
        })
        .resource_policy(workload.limits.policy())
        .feature(
            lspf::features::document_symbol(DocumentSymbolOptions::default()),
            partial_result_burst,
        )
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

        writes_blocked.store(true, Ordering::Release);
        input.send(request(
            2,
            "textDocument/documentSymbol",
            &json!({
                "textDocument": {"uri": "file:///soak.rs"},
                "partialResultToken": "soak-partial-results"
            }),
        )?)?;
        let (accepted, overloaded) = completion.await?;
        if overloaded == 0 {
            return Err("partial-result burst did not exercise outbound overload".into());
        }
        if accepted == 0 {
            return Err("partial-result burst admitted no chunks".into());
        }
        if accepted > workload.limits.outbound_messages {
            return Err("partial-result burst exceeded the outbound message budget".into());
        }
        context.sample_now()?;
        writes_blocked.store(false, Ordering::Release);
        write_release.add_permits(1);
        let first = tokio::time::timeout(Duration::from_secs(5), output.recv())
            .await?
            .ok_or("partial-result transport closed before delivery")?;
        let RawMessage::Notification { method, params } = first else {
            return Err(format!("unexpected first partial-result traffic: {first:?}").into());
        };
        if method.as_ref() != "$/progress" {
            return Err("partial-result burst did not emit progress first".into());
        }
        let message: Value = serde_json::from_slice(&params)?;
        if message["token"] != "soak-partial-results" {
            return Err("partial-result burst changed its progress token".into());
        }
        bytes += u64::try_from(params.len())?;
        release.notify_one();

        let mut progress_chunks = 1_usize;
        let mut completed_request = false;
        while progress_chunks < accepted || !completed_request {
            match tokio::time::timeout(Duration::from_secs(5), output.recv())
                .await?
                .ok_or("partial-result transport closed before delivery")?
            {
                RawMessage::Notification { method, params } if method.as_ref() == "$/progress" => {
                    let message: Value = serde_json::from_slice(&params)?;
                    if message["token"] != "soak-partial-results" {
                        return Err("partial-result burst changed its progress token".into());
                    }
                    progress_chunks += 1;
                    bytes += u64::try_from(params.len())?;
                }
                RawMessage::Response {
                    id: lspf::RequestId::Number(2),
                    result: Ok(result),
                } => {
                    if serde_json::from_slice::<Value>(&result)? != Value::Null {
                        return Err("partial-result request returned a non-null result".into());
                    }
                    completed_request = true;
                }
                other => {
                    return Err(format!("unexpected partial-result traffic: {other:?}").into());
                }
            }
        }
        operations += u64::try_from(accepted + overloaded)?;
        drop(input);
        let outcome = tokio::time::timeout(Duration::from_secs(5), serving).await???;
        if outcome != Outcome::TransportClosed {
            return Err(format!("partial-result outcome was {outcome:?}").into());
        }
        context.counts().connections.fetch_sub(1, Ordering::AcqRel);
        context.sample()?;
    }
    context.finish("transport_closed", operations, bytes)
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
            // Register the waiter before sampling the counter. `notify_waiters`
            // stores no permit, so a hook that lands between an unregistered
            // load and the following await is dropped; when that hook is the
            // batch's last, this loop waits forever and the timeout above turns
            // it into "progress scenario failed: deadline has elapsed".
            let mut notified = std::pin::pin!(observation.hook_notify.notified());
            loop {
                notified.as_mut().enable();
                if observation.hooks_seen.load(Ordering::Acquire) >= expected_hooks {
                    break;
                }
                notified.as_mut().await;
                notified.set(observation.hook_notify.notified());
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
