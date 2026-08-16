//! Adapter-neutral, wire-observed Transport conformance journey.
//!
//! First-party adapters provide only a wire client and a running real
//! [`Server`]. The journey never reaches into `ProtocolEngine` state.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lsp_types::notification::Notification;
use lsp_types::request::Request;
use lsp_types::{LogMessageParams, MessageType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Context, Outcome, Server};

pub(crate) trait WireClient {
    fn send(&mut self, message: Value) -> impl Future<Output = ()> + crate::TaskSend;
    fn receive(&mut self) -> impl Future<Output = Value> + crate::TaskSend;
}

#[derive(Debug, Deserialize, Serialize)]
struct ObserveParams {
    sequence: usize,
}

enum Observe {}

impl Notification for Observe {
    type Params = ObserveParams;
    const METHOD: &'static str = "conformance/observe";
}

#[derive(Debug, Deserialize, Serialize)]
struct JourneyParams {
    value: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct JourneyResult {
    echoed: String,
    observed_sequence: usize,
}

enum Journey {}

impl Request for Journey {
    type Params = JourneyParams;
    type Result = JourneyResult;
    const METHOD: &'static str = "conformance/journey";
}

enum EchoFromClient {}

impl Request for EchoFromClient {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "conformance/echoFromClient";
}

enum WaitForCancellation {}

impl Request for WaitForCancellation {
    type Params = Value;
    type Result = ();
    const METHOD: &'static str = "conformance/waitForCancellation";
}

pub(crate) fn server() -> Server<AtomicUsize> {
    Server::builder(AtomicUsize::new(0))
        .notification::<Observe, _, _>(
            |state: Arc<AtomicUsize>, ctx: Context, params: ObserveParams| async move {
                state.store(params.sequence, Ordering::SeqCst);
                ctx.client()
                    .log_message(LogMessageParams {
                        typ: MessageType::INFO,
                        message: "conformance observed".to_string(),
                    })
                    .expect("the conformance connection is open");
            },
        )
        .request::<Journey, _, _>(
            |state: Arc<AtomicUsize>, ctx: Context, params: JourneyParams, _ct| async move {
                let echoed = ctx
                    .client()
                    .request::<EchoFromClient>(params.value)
                    .await
                    .map_err(|error| crate::LspError::internal(error.to_string()))?;
                ctx.client()
                    .log_message(LogMessageParams {
                        typ: MessageType::INFO,
                        message: "conformance notification".to_string(),
                    })
                    .map_err(|error| crate::LspError::internal(error.to_string()))?;
                Ok(JourneyResult {
                    echoed,
                    observed_sequence: state.load(Ordering::SeqCst),
                })
            },
        )
        .request::<WaitForCancellation, _, _>(
            |_state: Arc<AtomicUsize>, _ctx, _params: Value, cancellation| async move {
                cancellation.cancelled().await;
                Ok(())
            },
        )
        .build()
        .expect("the conformance Server builds")
}

/// Run the single journey shared by every first-party Transport adapter.
pub(crate) async fn run<C, F>(client: &mut C, serving: F)
where
    C: WireClient,
    F: Future<Output = crate::Result<Outcome>>,
{
    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "processId": null, "rootUri": null, "capabilities": {} },
        }))
        .await;
    let initialized = client.receive().await;
    assert_eq!(initialized["id"], 1);

    client
        .send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }))
        .await;
    client
        .send(json!({
            "jsonrpc": "2.0",
            "method": "conformance/observe",
            "params": { "sequence": 7 },
        }))
        .await;
    let observed = client.receive().await;
    assert_eq!(observed["method"], "window/logMessage");
    assert_eq!(observed["params"]["message"], "conformance observed");
    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "conformance/journey",
            "params": { "value": "héllo" },
        }))
        .await;

    let outbound_request = client.receive().await;
    assert_eq!(outbound_request["method"], "conformance/echoFromClient");
    assert_eq!(outbound_request["params"], "héllo");
    let outbound_id = outbound_request["id"].clone();
    client
        .send(json!({ "jsonrpc": "2.0", "id": outbound_id, "result": "echoed" }))
        .await;

    let notification = client.receive().await;
    assert_eq!(notification["method"], "window/logMessage");
    assert_eq!(
        notification["params"]["message"],
        "conformance notification"
    );
    let journey = client.receive().await;
    assert_eq!(journey["id"], 2);
    assert_eq!(
        journey["result"],
        json!({ "echoed": "echoed", "observed_sequence": 7 })
    );

    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "conformance/waitForCancellation",
            "params": {},
        }))
        .await;
    client
        .send(json!({
            "jsonrpc": "2.0",
            "method": "$/cancelRequest",
            "params": { "id": 3 },
        }))
        .await;
    let cancelled = client.receive().await;
    assert_eq!(cancelled["id"], 3);
    assert_eq!(cancelled["error"]["code"], -32800);

    client
        .send(json!({ "jsonrpc": "2.0", "id": 4, "method": "shutdown" }))
        .await;
    let shutdown = client.receive().await;
    assert_eq!(shutdown["id"], 4);
    assert_eq!(shutdown["result"], Value::Null);
    client
        .send(json!({ "jsonrpc": "2.0", "method": "exit" }))
        .await;

    let outcome = serving
        .await
        .expect("the conformance journey serves without a transport error");
    assert_eq!(outcome, Outcome::Exit { code: 0 });
}
