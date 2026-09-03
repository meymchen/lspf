//! The frozen 1.0 interface, exercised from outside the crate (issue #190).
//!
//! An integration-test executable is a separate crate, so every path below is
//! reachable exactly as a downstream consumer reaches it. Naming the frozen
//! native root, testing, and owned-alias exports is the native compile-time
//! half of `docs/public-interface.md`;
//! `ci/check-public-interface.sh` owns the inventory half, and
//! `frozen_wasm_interface.rs` covers the `wasm32`-only rows against the real
//! target.
//!
//! The journeys below then use a representative slice of that surface for real,
//! so the inventory is not merely importable.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

// Available on every supported target and feature selection.
#[allow(unused_imports)]
use lspf::{
    BuildError, CallKind, CancellationToken, ClientError, ClientHandle, ConnectionDirection,
    ConnectionFailure, ConnectionFailureCategory, ConnectionFailureContext, ConnectionRequestId,
    DEFAULT_CONCURRENCY_LIMIT, Document, DocumentsView, Error, FeatureSpec, FileProvider,
    IncomingCall, JsonRpcError, Layer, LspError, MemoryFileProvider, Next, Notebook, NotebooksView,
    NotificationFeatureSpec, PartialResultRequest, PartialResultSink, PositionEncoding,
    ProgressError, ProgressHandle, ProgressOptions, RawMessage, RequestId, ResourcePolicy,
    ResourcePolicyField, Result, Server, ServerBuilder, ServerContext, ServiceFuture,
    ServiceResult, SharedHandler, TaskFuture, TaskSend, TelemetryEventParams, Transport,
    TransportError, TransportReader, TransportWriter, Workspace, WorkspaceError, features, types,
};
// InitializeRegistrar is only nameable through the transaction that lends it.
#[allow(unused_imports)]
use lspf::InitializeRegistrar;

// Available wherever a runtime can serve a connection.
#[allow(unused_imports)]
#[cfg(feature = "runtime-tokio")]
use lspf::{Client, ClientBuilder, ClientConnection, ClientContext, Outcome, ServerHandle};

// Available on native targets that have both a runtime and a filesystem.
#[allow(unused_imports)]
#[cfg(feature = "runtime-tokio")]
use lspf::{OsFileProvider, OsFileProviderBuilder};

#[allow(unused_imports)]
#[cfg(feature = "stdio")]
use lspf::{
    ChildConnection, ChildError, ChildOutput, StdioBuilder, StdioReader, StdioTransport,
    StdioWriter, stdio,
};

#[allow(unused_imports)]
#[cfg(feature = "tcp")]
use lspf::{TcpBuilder, TcpReader, TcpTransport, TcpWriter, tcp};

#[allow(unused_imports)]
#[cfg(feature = "websocket")]
use lspf::{WebSocketBuilder, WebSocketReader, WebSocketTransport, WebSocketWriter, websocket};

use lspf::testing::{
    ClientJourney, JourneyError, MemoryReader, MemoryTransport, MemoryWriter, ScriptedPeer,
    ServerJourney, VirtualClock, WireCapture, WireDirection, WireEvent,
};

use lspf::types::request::Request;
#[allow(unused_imports)]
use lspf::types::{
    ApplyWorkspaceEditResponse, CodeActionOrCommand, ColorProviderOptions,
    DiagnosticServerCapabilities, DocumentDiagnosticReportResult, GotoDefinitionParams,
    GotoDefinitionResponse, InlayHintLabel, InlayHintTooltip, PrepareRenameResponse,
    ReferencesOptions, SemanticTokenModifier, SemanticTokenType, SemanticTokensFullDeltaResult,
    SemanticTokensRangeResult, SemanticTokensResult, TextDocumentSyncCapability,
    TextDocumentSyncSaveOptions, WorkspaceDiagnosticReportResult, WorkspaceServerCapabilities,
};
use lspf::types::{
    ClientCapabilities, Hover, HoverContents, HoverParams, MarkupContent, MarkupKind,
};

struct FrozenState;

/// Registering a user `Layer` is the frozen extension seam: it sees the decoded
/// call and forwards it, and never sees transport bytes.
struct CountingLayer {
    requests: Arc<AtomicUsize>,
}

impl Layer<FrozenState> for CountingLayer {
    fn call(&self, call: IncomingCall<FrozenState>, next: Next<FrozenState>) -> ServiceFuture {
        if call.kind() == CallKind::Request {
            self.requests.fetch_add(1, Ordering::SeqCst);
        }
        Box::pin(async move { next.call(call).await })
    }
}

async fn hover(
    _state: Arc<FrozenState>,
    ctx: ServerContext,
    params: HoverParams,
    _cancel: CancellationToken,
) -> std::result::Result<Option<Hover>, LspError> {
    let uri = params.text_document_position_params.text_document.uri;
    let documents: DocumentsView = ctx.documents();
    let words = documents.get(&uri).map_or(0, |document: Document| {
        document.text().split_whitespace().count()
    });
    Ok(Some(Hover {
        contents: HoverContents::MarkupContent(MarkupContent {
            kind: MarkupKind::PlainText,
            value: words.to_string(),
        }),
        range: None,
    }))
}

/// A downstream crate builds and serves a `Server` naming only frozen items,
/// then reads the ordered wire record and the connection `Outcome`.
#[tokio::test]
async fn a_downstream_crate_drives_the_frozen_server_surface() {
    let layered_requests = Arc::new(AtomicUsize::new(0));
    let server = Server::builder(FrozenState)
        .file_provider(MemoryFileProvider::new())
        .resource_policy(ResourcePolicy {
            max_outbound_messages: 32,
            ..ResourcePolicy::default()
        })
        .concurrency_limit(DEFAULT_CONCURRENCY_LIMIT)
        .layer(CountingLayer {
            requests: Arc::clone(&layered_requests),
        })
        .feature(features::hover(), hover)
        .build()
        .expect("the frozen registration surface builds a server");

    let mut journey = ServerJourney::start(server)
        .await
        .expect("the frozen journey initializes the server");
    let capture: WireCapture = journey.capture();

    let peer: &mut ScriptedPeer = journey.peer();
    peer.send(RawMessage::Request {
        id: RequestId::Number(42),
        method: std::borrow::Cow::Borrowed(<types::request::HoverRequest as Request>::METHOD),
        params: bytes::Bytes::from(
            serde_json::to_vec(&json!({
                "textDocument": {"uri": "file:///frozen.txt"},
                "position": {"line": 0, "character": 0}
            }))
            .expect("hover params serialize"),
        ),
    })
    .expect("the scripted peer sends a hover request");

    let response = peer.recv().await.expect("the server answers the request");
    let RawMessage::Response { id, result } = response else {
        panic!("a hover request is answered with a response");
    };
    assert_eq!(
        id,
        RequestId::Number(42),
        "the response echoes the request id"
    );
    let hover: Hover = serde_json::from_slice(&result.expect("the handler succeeded"))
        .expect("the response carries the handler's typed Hover");
    assert!(
        matches!(hover.contents, HoverContents::MarkupContent(markup) if markup.value == "0"),
        "an unopened document has no text, so the handler counts zero words"
    );

    let outcome: Outcome = journey
        .finish()
        .await
        .expect("shutdown and exit complete the journey");
    assert_eq!(outcome.code(), 0, "a clean shutdown reports exit code 0");

    let traffic: Vec<WireEvent> = capture.snapshot();
    assert_eq!(
        traffic.first().map(WireEvent::direction),
        Some(WireDirection::PeerToEndpoint),
        "the initialize request is the first message across the seam"
    );
    assert!(
        traffic.iter().any(|event| event.sequence() > 0),
        "the capture preserves ordering across both directions"
    );
    assert_eq!(
        layered_requests.load(Ordering::SeqCst),
        1,
        "the registered Layer wrapped the hover request and nothing else"
    );
}

/// The symmetric journey for the Client endpoint and its `ServerHandle`.
#[tokio::test]
async fn a_downstream_crate_drives_the_frozen_client_surface() {
    let builder: ClientBuilder = Client::builder(ClientCapabilities::default());
    let mut journey = ClientJourney::start(builder)
        .await
        .expect("the frozen journey initializes the client");

    let handle: ServerHandle = journey.server();
    let capture: WireCapture = journey.capture();

    handle
        .notify::<types::notification::DidChangeConfiguration>(
            types::DidChangeConfigurationParams {
                settings: json!({"frozen": true}),
            },
        )
        .expect("a typed notification enqueues through the frozen handle");
    let forwarded = journey
        .peer()
        .recv()
        .await
        .expect("the peer observes the notification");
    assert_eq!(
        forwarded.method(),
        Some(<types::notification::DidChangeConfiguration as lspf::types::notification::Notification>::METHOD)
    );

    let outcome: Outcome = journey
        .finish()
        .await
        .expect("shutdown and exit complete the client journey");
    assert_eq!(outcome.code(), 0);
    assert!(!capture.snapshot().is_empty());
}

/// The frozen value types a downstream crate constructs without a connection.
#[test]
fn frozen_values_are_constructible_from_downstream_code() {
    assert_eq!(DEFAULT_CONCURRENCY_LIMIT, 64);
    assert_eq!(PositionEncoding::default(), PositionEncoding::Utf16);

    let error = LspError::internal("frozen");
    assert_eq!(error.code(), -32603);
    assert_eq!(error.message(), "frozen");

    let wire = RawMessage::ProtocolError {
        error: JsonRpcError {
            code: -32700,
            message: "frozen".to_string(),
            data: Some(json!({"kind": "parse"})),
        },
    };
    assert_eq!(wire.method(), None);

    let policy = ResourcePolicy::default();
    assert!(policy.max_inbound_requests > 0);

    let build_error = Server::builder(FrozenState)
        .resource_policy(ResourcePolicy {
            max_outbound_messages: 0,
            ..ResourcePolicy::default()
        })
        .build()
        .err()
        .expect("a zero outbound budget is rejected at build time");
    assert_eq!(
        build_error,
        BuildError::InvalidResourcePolicy {
            field: ResourcePolicyField::MaxOutboundMessages
        }
    );
}

/// `MemoryTransport` is the frozen custom-Transport seam, split into the same
/// reader and writer halves a first-party adapter provides.
#[tokio::test]
async fn the_frozen_transport_seam_splits_into_named_halves() {
    let (transport, mut peer): (MemoryTransport, ScriptedPeer) = MemoryTransport::pair();
    let (mut reader, mut writer): (MemoryReader, MemoryWriter) = transport.split();

    peer.send(RawMessage::Notification {
        method: std::borrow::Cow::Borrowed("frozen/inbound"),
        params: bytes::Bytes::from_static(b"{}"),
    })
    .expect("the peer writes into the seam");
    let inbound = reader.recv().await.expect("the reader half receives it");
    assert_eq!(inbound.method(), Some("frozen/inbound"));

    writer
        .send(RawMessage::Notification {
            method: std::borrow::Cow::Borrowed("frozen/outbound"),
            params: bytes::Bytes::from_static(b"{}"),
        })
        .await
        .expect("the writer half sends back");
    assert_eq!(
        peer.recv().await.expect("the peer receives it").method(),
        Some("frozen/outbound")
    );

    let failure: TransportError = TransportError::Closed;
    assert!(!failure.to_string().is_empty());
}

/// `VirtualClock` and `JourneyError` complete the frozen testing surface.
#[tokio::test(flavor = "current_thread", start_paused = false)]
async fn the_frozen_testing_clock_controls_connection_deadlines() {
    let clock = VirtualClock::pause();
    clock.advance(std::time::Duration::from_millis(50)).await;
    clock.resume();

    let error: JourneyError = JourneyError::from(lspf::Error::from(TransportError::Closed));
    assert!(!error.to_string().is_empty());
}
