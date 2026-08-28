//! Public protocol-testing utilities (issue #182).

use std::borrow::Cow;

use bytes::Bytes;
use lspf::testing::{ClientJourney, MemoryTransport, ServerJourney, VirtualClock, WireDirection};
use lspf::types::ClientCapabilities;
use lspf::types::request::Request;
use lspf::{
    Client, ClientError, Outcome, RawMessage, ResourcePolicy, Server, Transport, TransportReader,
    TransportWriter,
};

enum NeverReply {}

impl Request for NeverReply {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "test/neverReply";
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
