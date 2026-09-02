//! Public protocol-testing utilities (issue #182).

use std::borrow::Cow;

use bytes::Bytes;
use lspf::testing::{ClientJourney, MemoryTransport, ServerJourney, VirtualClock, WireDirection};
use lspf::types::ClientCapabilities;
use lspf::types::request::Request;
use lspf::{
    Client, ClientError, LspError, Outcome, ProgressOptions, RawMessage, RequestId, ResourcePolicy,
    Server, ServerContext, Transport, TransportReader, TransportWriter,
};
use serde_json::{Value, json};

enum NeverReply {}

impl Request for NeverReply {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "test/neverReply";
}

enum AdoptWorkDoneToken {}

impl Request for AdoptWorkDoneToken {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = "test/adoptWorkDoneToken";
}

fn notification(method: &'static str) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from_static(b"{}"),
    }
}

#[tokio::test]
async fn peer_scripts_messages_and_capture_keeps_cross_direction_order() {
    let (transport, mut peer) = MemoryTransport::pair();
    let capture = peer.capture();
    let (mut reader, mut writer) = transport.split();

    peer.send(notification("test/inbound")).unwrap();
    assert_eq!(reader.recv().await.unwrap().method(), Some("test/inbound"));

    writer.send(notification("test/outbound")).await.unwrap();
    assert_eq!(peer.recv().await.unwrap().method(), Some("test/outbound"));

    let traffic = capture.snapshot();
    assert_eq!(traffic.len(), 2);
    assert_eq!(traffic[0].sequence(), 0);
    assert_eq!(traffic[0].direction(), WireDirection::PeerToEndpoint);
    assert_eq!(traffic[0].message().method(), Some("test/inbound"));
    assert_eq!(traffic[1].sequence(), 1);
    assert_eq!(traffic[1].direction(), WireDirection::EndpointToPeer);
    assert_eq!(traffic[1].message().method(), Some("test/outbound"));
}

#[tokio::test]
async fn capture_excludes_messages_that_never_cross_the_transport_seam() {
    let (transport, peer) = MemoryTransport::pair();
    let capture = peer.capture();
    let (reader, mut writer) = transport.split();

    drop(reader);
    assert!(peer.send(notification("test/not-delivered")).is_err());
    drop(peer);
    assert!(
        writer
            .send(notification("test/not-observed"))
            .await
            .is_err()
    );

    assert!(capture.snapshot().is_empty());
}

#[tokio::test]
async fn uncaptured_pair_forwards_messages_without_retaining_wire_history() {
    let (transport, mut peer) = MemoryTransport::pair_uncaptured();
    let capture = peer.capture();
    let (mut reader, mut writer) = transport.split();

    peer.send(notification("test/inbound")).unwrap();
    assert_eq!(reader.recv().await.unwrap().method(), Some("test/inbound"));

    writer.send(notification("test/outbound")).await.unwrap();
    assert_eq!(peer.recv().await.unwrap().method(), Some("test/outbound"));

    assert!(capture.snapshot().is_empty());
}

#[tokio::test]
async fn peer_scripts_transport_failures() {
    let (transport, peer) = MemoryTransport::pair();
    let (mut reader, _writer) = transport.split();

    peer.fail(lspf::TransportError::Malformed("scripted failure".into()))
        .unwrap();
    assert!(matches!(
        reader.recv().await,
        Err(lspf::TransportError::Malformed(message)) if message == "scripted failure"
    ));
}

#[tokio::test]
async fn server_journey_runs_the_reusable_public_lifecycle() {
    let server = Server::builder(()).build().unwrap();
    let journey = ServerJourney::start(server).await.unwrap();
    let capture = journey.capture();

    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });

    let methods: Vec<_> = capture
        .snapshot()
        .into_iter()
        .filter_map(|event| event.message().method().map(str::to_owned))
        .collect();
    assert_eq!(methods, ["initialize", "initialized", "shutdown", "exit"]);
}

#[tokio::test]
async fn server_context_adopts_the_requests_work_done_token() {
    let server = Server::builder(())
        .request::<AdoptWorkDoneToken, _, _>(
            |_state, ctx: ServerContext, _params, _cancellation| async move {
                let progress = ctx
                    .begin_progress(
                        ProgressOptions::new("Indexing")
                            .cancellable(true)
                            .message("starting")
                            .percentage(0),
                    )
                    .map_err(LspError::internal)?
                    .expect("the request supplied a work-done token");
                progress
                    .report(Some("half".into()), Some(50))
                    .map_err(LspError::internal)?;
                let token = serde_json::to_value(progress.token()).map_err(LspError::internal)?;
                progress
                    .end(Some("done".into()))
                    .map_err(LspError::internal)?;
                Ok(token)
            },
        )
        .build()
        .unwrap();
    let mut journey = ServerJourney::start(server).await.unwrap();

    journey
        .peer()
        .send(RawMessage::Request {
            id: RequestId::Number(9),
            method: Cow::Borrowed(AdoptWorkDoneToken::METHOD),
            params: Bytes::from_static(br#"{"workDoneToken":"client-token"}"#),
        })
        .unwrap();

    let begin = journey.peer().recv().await.unwrap();
    assert_eq!(begin.method(), Some("$/progress"));
    let RawMessage::Notification { params, .. } = begin else {
        panic!("the adopted token must not require a progress-create request");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(&params).unwrap(),
        json!({
            "token": "client-token",
            "value": {
                "kind": "begin",
                "title": "Indexing",
                "cancellable": true,
                "message": "starting",
                "percentage": 0
            }
        })
    );

    let report = journey.peer().recv().await.unwrap();
    let RawMessage::Notification { params, .. } = report else {
        panic!("expected a progress report");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(&params).unwrap(),
        json!({
            "token": "client-token",
            "value": {
                "kind": "report",
                "cancellable": true,
                "message": "half",
                "percentage": 50
            }
        })
    );

    let end = journey.peer().recv().await.unwrap();
    let RawMessage::Notification { params, .. } = end else {
        panic!("expected a progress end");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(&params).unwrap(),
        json!({
            "token": "client-token",
            "value": { "kind": "end", "message": "done" }
        })
    );

    let response = journey.peer().recv().await.unwrap();
    assert!(matches!(
        response,
        RawMessage::Response {
            id: RequestId::Number(9),
            result: Ok(ref result),
        } if serde_json::from_slice::<Value>(result).unwrap() == json!("client-token")
    ));

    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });
}

#[tokio::test]
async fn server_context_has_no_progress_handle_without_a_request_token() {
    let server = Server::builder(())
        .request::<AdoptWorkDoneToken, _, _>(
            |_state, ctx: ServerContext, _params, _cancellation| async move {
                let progress = ctx
                    .begin_progress(ProgressOptions::new("unused"))
                    .map_err(LspError::internal)?;
                Ok(json!(progress.is_some()))
            },
        )
        .build()
        .unwrap();
    let mut journey = ServerJourney::start(server).await.unwrap();

    journey
        .peer()
        .send(RawMessage::Request {
            id: RequestId::Number(9),
            method: Cow::Borrowed(AdoptWorkDoneToken::METHOD),
            params: Bytes::from_static(b"{}"),
        })
        .unwrap();

    let response = journey.peer().recv().await.unwrap();
    assert!(matches!(
        response,
        RawMessage::Response {
            id: RequestId::Number(9),
            result: Ok(ref result),
        } if serde_json::from_slice::<Value>(result).unwrap() == json!(false)
    ));

    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });
}

#[test]
fn reserved_lsp_errors_preserve_their_codes_and_messages() {
    let request_failed = LspError::RequestFailed("index unavailable".into());
    assert_eq!(request_failed.code(), -32803);
    assert_eq!(request_failed.message(), "index unavailable");

    let server_cancelled = LspError::ServerCancelled("handler deadline expired".into());
    assert_eq!(server_cancelled.code(), -32802);
    assert_eq!(server_cancelled.message(), "handler deadline expired");
}

#[tokio::test]
async fn client_journey_runs_the_reusable_public_lifecycle() {
    let journey = ClientJourney::start(Client::builder(ClientCapabilities::default()))
        .await
        .unwrap();
    let capture = journey.capture();

    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });

    let methods: Vec<_> = capture
        .snapshot()
        .into_iter()
        .filter_map(|event| event.message().method().map(str::to_owned))
        .collect();
    assert_eq!(methods, ["initialize", "initialized", "shutdown", "exit"]);
}

#[tokio::test]
async fn virtual_clock_makes_a_real_request_timeout_deterministic() {
    let clock = VirtualClock::pause();
    let journey = ClientJourney::start(
        Client::builder(ClientCapabilities::default()).resource_policy(ResourcePolicy {
            outbound_request_timeout: Some(std::time::Duration::from_secs(5)),
            ..ResourcePolicy::default()
        }),
    )
    .await
    .unwrap();
    let server = journey.server();
    let request = tokio::spawn(async move { server.request::<NeverReply>(()).await });
    let mut journey = journey;

    assert_eq!(
        journey.peer().recv().await.unwrap().method(),
        Some(NeverReply::METHOD)
    );
    clock.advance(std::time::Duration::from_secs(5)).await;
    assert!(matches!(request.await.unwrap(), Err(ClientError::Timeout)));
    assert_eq!(
        journey.peer().recv().await.unwrap().method(),
        Some("$/cancelRequest")
    );

    assert_eq!(journey.finish().await.unwrap(), Outcome::Exit { code: 0 });
}
