//! Typed outgoing request helpers on `ClientHandle` (issues #104, #105, #106, #107).
//!
//! `show_document`, `show_message_request`, and `apply_edit` are thin wrappers
//! over the generic typed request broker, as are the client-owned workspace
//! queries `configuration` and `workspace_folders`, the dynamic capability
//! announcements `register_capability` and `unregister_capability`, and the
//! five stable workspace refresh helpers `refresh_code_lenses`,
//! `refresh_diagnostics`, `refresh_inlay_hints`, `refresh_inline_values`, and
//! `refresh_semantic_tokens`. With the `proposed` Cargo feature the same
//! coverage extends to the proposed `refresh_folding_ranges` and
//! `refresh_text_document_content` helpers (issue #108). These
//! wire-level tests run each helper inside a real handler over an in-memory
//! transport, pin the outgoing request's method and parameter shape against
//! the fixtures under `tests/fixtures/`, and then complete it through every
//! documented path: success, a remote JSON-RPC error, an invalid success
//! result, caller abandonment, and connection disconnect. A final integration
//! test proves that dynamic registration never makes an otherwise absent
//! local route dispatchable.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use lspf::types::request::Request;
use lspf::types::{
    ApplyWorkspaceEditResponse, MessageActionItem, ShowDocumentResult, WorkspaceFolder,
};
use lspf::{
    CancellationToken, ClientError, ClientHandle, RawMessage, RequestId, Server, ServerContext,
    Transport, TransportError, TransportReader, TransportWriter,
};
use serde_json::json;
use tokio::sync::mpsc;

const SHOW_DOCUMENT_FIXTURE: &str = include_str!("fixtures/show_document_request.json");
const SHOW_MESSAGE_REQUEST_FIXTURE: &str = include_str!("fixtures/show_message_request.json");
const APPLY_EDIT_FIXTURE: &str = include_str!("fixtures/apply_edit_request.json");
const CONFIGURATION_FIXTURE: &str = include_str!("fixtures/configuration_request.json");
const WORKSPACE_FOLDERS_FIXTURE: &str = include_str!("fixtures/workspace_folders_request.json");
const REGISTER_CAPABILITY_FIXTURE: &str = include_str!("fixtures/register_capability_request.json");
const UNREGISTER_CAPABILITY_FIXTURE: &str =
    include_str!("fixtures/unregister_capability_request.json");
const CODE_LENS_REFRESH_FIXTURE: &str = include_str!("fixtures/code_lens_refresh_request.json");
const DIAGNOSTIC_REFRESH_FIXTURE: &str = include_str!("fixtures/diagnostic_refresh_request.json");
const INLAY_HINT_REFRESH_FIXTURE: &str = include_str!("fixtures/inlay_hint_refresh_request.json");
const INLINE_VALUE_REFRESH_FIXTURE: &str =
    include_str!("fixtures/inline_value_refresh_request.json");
const SEMANTIC_TOKENS_REFRESH_FIXTURE: &str =
    include_str!("fixtures/semantic_tokens_refresh_request.json");

// --- Registrations -----------------------------------------------------------

/// A probe request whose handler runs one outgoing helper call. The trigger's
/// params are the helper's params as JSON; its result is the handler's marker
/// string, which synchronizes the test with the read loop.
enum Trigger {}

impl Request for Trigger {
    type Params = serde_json::Value;
    type Result = String;
    const METHOD: &'static str = "test/trigger";
}

// --- Harness -----------------------------------------------------------------

struct ChannelTransport {
    incoming: mpsc::UnboundedReceiver<RawMessage>,
    outgoing: mpsc::UnboundedSender<RawMessage>,
}

struct ChannelReader(mpsc::UnboundedReceiver<RawMessage>);
struct ChannelWriter(mpsc::UnboundedSender<RawMessage>);

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (ChannelReader(self.incoming), ChannelWriter(self.outgoing))
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.0.recv().await.ok_or(TransportError::Closed)
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        self.0.send(message).map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

struct Session {
    in_tx: mpsc::UnboundedSender<RawMessage>,
    out_rx: mpsc::UnboundedReceiver<RawMessage>,
    serve: tokio::task::JoinHandle<lspf::Result<lspf::Outcome>>,
}

fn inbound_request(id: i32, method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn success_response(id: i32, result: serde_json::Value) -> RawMessage {
    RawMessage::Response {
        id: RequestId::Number(id),
        result: Ok(Bytes::from(serde_json::to_vec(&result).unwrap())),
    }
}

fn error_response(
    id: i32,
    code: i32,
    message: &'static str,
    data: Option<serde_json::Value>,
) -> RawMessage {
    RawMessage::Response {
        id: RequestId::Number(id),
        result: Err(lspf::JsonRpcError {
            code,
            message: message.to_string(),
            data,
        }),
    }
}

fn exit() -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed("exit"),
        params: Bytes::from_static(b"null"),
    }
}

async fn receive(outgoing: &mut mpsc::UnboundedReceiver<RawMessage>) -> RawMessage {
    tokio::time::timeout(std::time::Duration::from_secs(2), outgoing.recv())
        .await
        .expect("server output before watchdog timeout")
        .expect("server output channel remains open")
}

async fn start(server: Server<()>) -> Session {
    let (in_tx, incoming) = mpsc::unbounded_channel();
    let (outgoing, out_rx) = mpsc::unbounded_channel();
    let serve = tokio::spawn(server.serve(ChannelTransport { incoming, outgoing }));
    Session {
        in_tx,
        out_rx,
        serve,
    }
}

async fn init(session: &mut Session) {
    session
        .in_tx
        .send(inbound_request(
            1,
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        ))
        .unwrap();
    let response = receive(&mut session.out_rx).await;
    assert_eq!(response.id(), Some(&RequestId::Number(1)));
}

async fn finish(session: Session) {
    session.in_tx.send(exit()).unwrap();
    session
        .serve
        .await
        .expect("serve task did not panic")
        .expect("serve ended cleanly");
}

fn result_string(response: &RawMessage) -> String {
    match response {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => serde_json::from_slice(bytes).expect("the result decodes"),
        other => panic!("expected a success response, got {other:?}"),
    }
}

fn assert_wire_request(message: &RawMessage, fixture: &str) {
    let RawMessage::Request { method, params, .. } = message else {
        panic!("expected a request, got {message:?}")
    };
    let mut wire = json!({ "method": method.as_ref() });
    if !params.is_empty() {
        wire["params"] = serde_json::from_slice(params).expect("the params are valid JSON");
    }
    let expected: serde_json::Value =
        serde_json::from_str(fixture).expect("the fixture is valid JSON");
    assert_eq!(wire, expected, "the wire shape must match the fixture");
}

/// The numeric ID of an outbound request, which is how the mock peer answers
/// it and how a `$/cancelRequest` names it.
fn outbound_number_id(message: &RawMessage) -> i32 {
    match message {
        RawMessage::Request { id, .. } => match id {
            RequestId::Number(n) => *n,
            _ => panic!("expected a numeric outbound request id"),
        },
        other => panic!("expected a request, got {other:?}"),
    }
}

fn take_outcome<T>(outcome: &Arc<Mutex<Option<T>>>) -> T {
    outcome
        .lock()
        .unwrap()
        .take()
        .expect("the handler recorded an outcome")
}

/// Poll for an outcome recorded by a detached task, which survives the
/// session close that completes the pending request.
async fn await_outcome<T>(outcome: &Arc<Mutex<Option<T>>>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(value) = outcome.lock().unwrap().take() {
                return value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the detached task recorded an outcome within the watchdog")
}

// --- Per-helper behavior tests ----------------------------------------------

/// Instantiate the five wire-level behavior tests for one helper. Each test
/// module gets:
///
/// - `success_matches_the_wire_fixture_and_decodes_the_result`
/// - `remote_error_preserves_code_message_and_data`
/// - `invalid_success_result_reports_deserialize`
/// - `abandoning_the_future_cancels_the_request_on_the_wire`
/// - `connection_disconnect_resolves_the_pending_request_as_cancelled`
///
/// `call` is an expression producing the helper future given a `ClientHandle` and
/// the trigger params as `serde_json::Value`. It is invoked three ways: awaited
/// inside the trigger handler, dropped without awaiting after a short delay
/// (abandonment), and awaited on a detached task that survives session close.
macro_rules! helper_wire_tests {
    (
        $name:ident,
        result = $result:ty,
        params = $params:expr,
        fixture = $fixture:ident,
        call = $call:expr,
        success_reply = $success_reply:expr,
        success_assert = $success_assert:expr,
        invalid_reply = $invalid_reply:expr $(,)?
    ) => {
        mod $name {
            use super::*;

            /// The helper awaited to completion inside the trigger handler.
            fn awaited_call(
                client: ClientHandle,
                params: serde_json::Value,
            ) -> impl std::future::Future<Output = Result<$result, ClientError>> {
                async move { ($call)(client, params).await }
            }

            /// The helper's future created and dropped without being awaited.
            async fn abandoned_call(client: ClientHandle, params: serde_json::Value) {
                let fut = ($call)(client, params);
                tokio::select! {
                    _ = fut => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
                }
            }

            fn roundtrip_server(
                outcome: &Arc<Mutex<Option<Result<$result, ClientError>>>>,
            ) -> Server<()> {
                let outcome = Arc::clone(outcome);
                Server::builder(())
                    .request::<Trigger, _, _>(move |_state: Arc<()>,
                                                 ctx: ServerContext,
                                                 params: serde_json::Value,
                                                 _ct: CancellationToken| {
                        let outcome = Arc::clone(&outcome);
                        async move {
                            let result = awaited_call(ctx.client(), params).await;
                            *outcome.lock().unwrap() = Some(result);
                            Ok("triggered".to_string())
                        }
                    })
                    .build()
                    .expect("the outgoing-request server builds")
            }

            fn abandon_server() -> Server<()> {
                Server::builder(())
                    .request::<Trigger, _, _>(move |_state: Arc<()>,
                                                 ctx: ServerContext,
                                                 params: serde_json::Value,
                                                 _ct: CancellationToken| {
                        async move {
                            abandoned_call(ctx.client(), params).await;
                            Ok("abandoned".to_string())
                        }
                    })
                    .build()
                    .expect("the outgoing-request server builds")
            }

            fn detached_server(
                outcome: &Arc<Mutex<Option<Result<$result, ClientError>>>>,
            ) -> Server<()> {
                let outcome = Arc::clone(outcome);
                Server::builder(())
                    .request::<Trigger, _, _>(move |_state: Arc<()>,
                                                 ctx: ServerContext,
                                                 params: serde_json::Value,
                                                 _ct: CancellationToken| {
                        let outcome = Arc::clone(&outcome);
                        async move {
                            tokio::spawn(async move {
                                let result = awaited_call(ctx.client(), params).await;
                                *outcome.lock().unwrap() = Some(result);
                            });
                            Ok("spawned".to_string())
                        }
                    })
                    .build()
                    .expect("the outgoing-request server builds")
            }

            /// Send the trigger, return the outbound request the helper put on
            /// the wire (with its wire shape pinned against the fixture).
            async fn trigger_request(session: &mut Session) -> i32 {
                session
                    .in_tx
                    .send(inbound_request(2, Trigger::METHOD, $params))
                    .unwrap();
                let outbound = receive(&mut session.out_rx).await;
                assert_wire_request(&outbound, $fixture);
                outbound_number_id(&outbound)
            }

            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn success_matches_the_wire_fixture_and_decodes_the_result() {
                let outcome = Arc::new(Mutex::new(None));
                let mut session = start(roundtrip_server(&outcome)).await;
                init(&mut session).await;

                let id = trigger_request(&mut session).await;
                session
                    .in_tx
                    .send(success_response(id, $success_reply))
                    .unwrap();
                let response = receive(&mut session.out_rx).await;
                assert_eq!(response.id(), Some(&RequestId::Number(2)));
                assert_eq!(result_string(&response), "triggered");
                finish(session).await;

                let value = take_outcome(&outcome).expect("the helper succeeded");
                ($success_assert)(value);
            }

            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn remote_error_preserves_code_message_and_data() {
                let outcome = Arc::new(Mutex::new(None));
                let mut session = start(roundtrip_server(&outcome)).await;
                init(&mut session).await;

                let id = trigger_request(&mut session).await;
                session
                    .in_tx
                    .send(error_response(
                        id,
                        -32001,
                        "test error",
                        Some(json!({ "detail": "transient" })),
                    ))
                    .unwrap();
                let response = receive(&mut session.out_rx).await;
                assert_eq!(response.id(), Some(&RequestId::Number(2)));
                finish(session).await;

                let err = take_outcome(&outcome).unwrap_err();
                match err {
                    ClientError::Remote(e) => {
                        assert_eq!(e.code, -32001);
                        assert_eq!(e.message, "test error");
                        assert_eq!(e.data, Some(json!({ "detail": "transient" })));
                    }
                    other => panic!("expected ClientError::Remote, got {other:?}"),
                }
            }

            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn invalid_success_result_reports_deserialize() {
                let outcome = Arc::new(Mutex::new(None));
                let mut session = start(roundtrip_server(&outcome)).await;
                init(&mut session).await;

                let id = trigger_request(&mut session).await;
                session
                    .in_tx
                    .send(success_response(id, $invalid_reply))
                    .unwrap();
                let response = receive(&mut session.out_rx).await;
                assert_eq!(response.id(), Some(&RequestId::Number(2)));
                finish(session).await;

                let err = take_outcome(&outcome).unwrap_err();
                assert!(
                    matches!(err, ClientError::Deserialize(_)),
                    "expected ClientError::Deserialize, got {err:?}"
                );
            }

            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn abandoning_the_future_cancels_the_request_on_the_wire() {
                let mut session = start(abandon_server()).await;
                init(&mut session).await;

                session
                    .in_tx
                    .send(inbound_request(2, Trigger::METHOD, $params))
                    .unwrap();
                let outbound = receive(&mut session.out_rx).await;
                let id = outbound_number_id(&outbound);

                // Dropping the future emits exactly one typed $/cancelRequest
                // naming the abandoned request's ID.
                let cancel = receive(&mut session.out_rx).await;
                match cancel {
                    RawMessage::Notification { method, params } => {
                        assert_eq!(&*method, "$/cancelRequest");
                        let params: serde_json::Value =
                            serde_json::from_slice(&params).unwrap();
                        assert_eq!(params["id"], serde_json::json!(id));
                    }
                    other => panic!("expected a $/cancelRequest notification, got {other:?}"),
                }

                // The handler still completes normally.
                let response = receive(&mut session.out_rx).await;
                assert_eq!(response.id(), Some(&RequestId::Number(2)));
                assert_eq!(result_string(&response), "abandoned");
                finish(session).await;
            }

            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn connection_disconnect_resolves_the_pending_request_as_cancelled() {
                let outcome = Arc::new(Mutex::new(None));
                let mut session = start(detached_server(&outcome)).await;
                init(&mut session).await;

                session
                    .in_tx
                    .send(inbound_request(2, Trigger::METHOD, $params))
                    .unwrap();
                // The trigger response may race the detached task's request;
                // skip it until the request itself appears on the wire, which
                // proves it is pending when the connection drops.
                loop {
                    match receive(&mut session.out_rx).await {
                        RawMessage::Request { .. } => break,
                        _ => {}
                    }
                }

                // Close the transport without answering: the pending request
                // must complete with Cancelled, and serving must not hang.
                drop(session.in_tx);
                tokio::time::timeout(std::time::Duration::from_secs(3), session.serve)
                    .await
                    .expect("serve returned within the watchdog")
                    .expect("serve task did not panic")
                    .expect("serve ended cleanly");

                let err = await_outcome(&outcome).await.unwrap_err();
                assert!(
                    matches!(err, ClientError::Cancelled),
                    "expected ClientError::Cancelled, got {err:?}"
                );
            }
        }
    };
}

helper_wire_tests!(
    show_document,
    result = ShowDocumentResult,
    params = json!({ "uri": "file:///guide.md", "takeFocus": true }),
    fixture = SHOW_DOCUMENT_FIXTURE,
    call = |client: ClientHandle, params: serde_json::Value| async move {
        client
            .show_document(serde_json::from_value(params).unwrap())
            .await
    },
    success_reply = json!({ "success": true }),
    success_assert =
        |value: ShowDocumentResult| assert_eq!(value, ShowDocumentResult { success: true }),
    invalid_reply = json!({ "success": "yes" }),
);

helper_wire_tests!(
    show_message_request,
    result = Option<MessageActionItem>,
    params = json!({
        "type": 3,
        "message": "save before closing?",
        "actions": [{ "title": "Save" }, { "title": "Discard" }],
    }),
    fixture = SHOW_MESSAGE_REQUEST_FIXTURE,
    call = |client: ClientHandle, params: serde_json::Value| async move {
        client
            .show_message_request(serde_json::from_value(params).unwrap())
            .await
    },
    success_reply = json!({ "title": "Save" }),
    success_assert = |value: Option<MessageActionItem>| {
        let item = value.expect("the user picked an action");
        assert_eq!(item.title, "Save");
        assert!(item.properties.is_empty());
    },
    invalid_reply = json!({ "title": 42 }),
);

helper_wire_tests!(
    apply_edit,
    result = ApplyWorkspaceEditResponse,
    params = json!({
        "label": "rename symbol",
        "edit": {
            "changes": {
                "file:///main.rs": [
                    {
                        "range": {
                            "start": { "line": 0, "character": 4 },
                            "end": { "line": 0, "character": 7 },
                        },
                        "newText": "renamed",
                    },
                ],
            },
        },
    }),
    fixture = APPLY_EDIT_FIXTURE,
    call = |client: ClientHandle, params: serde_json::Value| async move {
        client
            .apply_edit(serde_json::from_value(params).unwrap())
            .await
    },
    success_reply = json!({ "applied": true }),
    success_assert = |value: ApplyWorkspaceEditResponse| {
        assert_eq!(
            value,
            ApplyWorkspaceEditResponse {
                applied: true,
                failure_reason: None,
                failed_change: None,
            }
        );
    },
    invalid_reply = json!({ "applied": "yes" }),
);

helper_wire_tests!(
    configuration,
    result = Vec<serde_json::Value>,
    params = json!({
        "items": [
            { "section": "editor" },
            { "scopeUri": "file:///workspace/main.rs", "section": "lspf.language" },
        ],
    }),
    fixture = CONFIGURATION_FIXTURE,
    call = |client: ClientHandle, params: serde_json::Value| async move {
        client.configuration(serde_json::from_value(params).unwrap()).await
    },
    // The reply mixes a filled value, a null, and an extra entry: the result
    // must keep the client's order and length exactly, filling nothing in and
    // truncating nothing out.
    success_reply = json!([{ "tabSize": 4 }, null, { "extra": true }]),
    success_assert = |value: Vec<serde_json::Value>| {
        assert_eq!(
            value,
            vec![json!({ "tabSize": 4 }), serde_json::Value::Null, json!({ "extra": true })],
            "the client's order and length are preserved exactly"
        );
    },
    invalid_reply = json!({ "not": "an array" }),
);

helper_wire_tests!(
    workspace_folders,
    result = Option<Vec<WorkspaceFolder>>,
    params = json!(null),
    fixture = WORKSPACE_FOLDERS_FIXTURE,
    call = |client: ClientHandle, _params: serde_json::Value| async move { client.workspace_folders().await },
    success_reply = json!([
        { "uri": "file:///a", "name": "a" },
        { "uri": "file:///b", "name": "b" },
    ]),
    success_assert = |value: Option<Vec<WorkspaceFolder>>| {
        let folders = value.expect("the client answered with folders");
        assert_eq!(folders.len(), 2, "the client's order and length are preserved");
        assert_eq!(folders[0].uri.as_str(), "file:///a");
        assert_eq!(folders[0].name, "a");
        assert_eq!(folders[1].uri.as_str(), "file:///b");
        assert_eq!(folders[1].name, "b");
    },
    invalid_reply = json!([{ "name": "missing uri" }]),
);

helper_wire_tests!(
    register_capability,
    result = (),
    params = json!({
        "registrations": [
            {
                "id": "hover-registration",
                "method": "textDocument/hover",
                "registerOptions": { "documentSelector": [{ "language": "rust" }] },
            },
        ],
    }),
    fixture = REGISTER_CAPABILITY_FIXTURE,
    call = |client: ClientHandle, params: serde_json::Value| async move {
        client
            .register_capability(serde_json::from_value(params).unwrap())
            .await
    },
    success_reply = json!(null),
    success_assert = |value: ()| assert_eq!(value, ()),
    invalid_reply = json!({ "unexpected": true }),
);

helper_wire_tests!(
    unregister_capability,
    result = (),
    params = json!({
        "unregisterations": [
            { "id": "hover-registration", "method": "textDocument/hover" },
        ],
    }),
    fixture = UNREGISTER_CAPABILITY_FIXTURE,
    call = |client: ClientHandle, params: serde_json::Value| async move {
        client
            .unregister_capability(serde_json::from_value(params).unwrap())
            .await
    },
    success_reply = json!(null),
    success_assert = |value: ()| assert_eq!(value, ()),
    invalid_reply = json!({ "unexpected": true }),
);

helper_wire_tests!(
    refresh_code_lenses,
    result = (),
    params = json!(null),
    fixture = CODE_LENS_REFRESH_FIXTURE,
    call = |client: ClientHandle, _params: serde_json::Value| async move {
        client.refresh_code_lenses().await
    },
    success_reply = json!(null),
    success_assert = |value: ()| assert_eq!(value, ()),
    invalid_reply = json!({ "unexpected": true }),
);

helper_wire_tests!(
    refresh_diagnostics,
    result = (),
    params = json!(null),
    fixture = DIAGNOSTIC_REFRESH_FIXTURE,
    call = |client: ClientHandle, _params: serde_json::Value| async move {
        client.refresh_diagnostics().await
    },
    success_reply = json!(null),
    success_assert = |value: ()| assert_eq!(value, ()),
    invalid_reply = json!({ "unexpected": true }),
);

helper_wire_tests!(
    refresh_inlay_hints,
    result = (),
    params = json!(null),
    fixture = INLAY_HINT_REFRESH_FIXTURE,
    call = |client: ClientHandle, _params: serde_json::Value| async move {
        client.refresh_inlay_hints().await
    },
    success_reply = json!(null),
    success_assert = |value: ()| assert_eq!(value, ()),
    invalid_reply = json!({ "unexpected": true }),
);

helper_wire_tests!(
    refresh_inline_values,
    result = (),
    params = json!(null),
    fixture = INLINE_VALUE_REFRESH_FIXTURE,
    call = |client: ClientHandle, _params: serde_json::Value| async move {
        client.refresh_inline_values().await
    },
    success_reply = json!(null),
    success_assert = |value: ()| assert_eq!(value, ()),
    invalid_reply = json!({ "unexpected": true }),
);

helper_wire_tests!(
    refresh_semantic_tokens,
    result = (),
    params = json!(null),
    fixture = SEMANTIC_TOKENS_REFRESH_FIXTURE,
    call = |client: ClientHandle, _params: serde_json::Value| async move {
        client.refresh_semantic_tokens().await
    },
    success_reply = json!(null),
    success_assert = |value: ()| assert_eq!(value, ()),
    invalid_reply = json!({ "unexpected": true }),
);

// --- Proposed workspace refresh helpers (issue #108) --------------------------
//
// These helpers and their local protocol types exist only when the crate's
// `proposed` Cargo feature is enabled, so the whole module is compiled out of
// default builds.

#[cfg(feature = "proposed")]
mod proposed {
    use super::*;

    const FOLDING_RANGE_REFRESH_FIXTURE: &str =
        include_str!("fixtures/folding_range_refresh_request.json");
    const TEXT_DOCUMENT_CONTENT_REFRESH_FIXTURE: &str =
        include_str!("fixtures/text_document_content_refresh_request.json");

    helper_wire_tests!(
        refresh_folding_ranges,
        result = (),
        params = json!(null),
        fixture = FOLDING_RANGE_REFRESH_FIXTURE,
        call = |client: ClientHandle, _params: serde_json::Value| async move {
            client.refresh_folding_ranges().await
        },
        success_reply = json!(null),
        success_assert = |value: ()| assert_eq!(value, ()),
        invalid_reply = json!({ "unexpected": true }),
    );

    helper_wire_tests!(
        refresh_text_document_content,
        result = (),
        params = json!({ "uri": "file:///main.rs" }),
        fixture = TEXT_DOCUMENT_CONTENT_REFRESH_FIXTURE,
        call = |client: ClientHandle, params: serde_json::Value| async move {
            client
                .refresh_text_document_content(serde_json::from_value(params).unwrap())
                .await
        },
        success_reply = json!(null),
        success_assert = |value: ()| assert_eq!(value, ()),
        invalid_reply = json!({ "unexpected": true }),
    );

    /// The text-document-content refresh params carry only the target
    /// `DocumentUri`: the struct has exactly that one field, so the helper
    /// produces exactly the `uri` wire field.
    #[test]
    fn text_document_content_refresh_params_contain_only_the_uri() {
        let params: lspf::proposed::TextDocumentContentRefreshParams =
            serde_json::from_value(json!({ "uri": "file:///main.rs" })).unwrap();
        assert_eq!(params.uri.as_str(), "file:///main.rs");
        assert_eq!(
            serde_json::to_value(&params).unwrap(),
            json!({ "uri": "file:///main.rs" }),
            "the params serialize to exactly the uri field"
        );
    }
}

// --- Router freeze integration test (issue #106) ----------------------------

/// A dynamic `client/registerCapability` announcement tells the client about
/// a capability; it never adds a route to the connection's frozen Router.
/// Dynamically registering `textDocument/hover` must not make the otherwise
/// absent local hover route dispatchable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dynamic_registration_does_not_dispatch_an_absent_route() {
    // The trigger handler announces hover support to the client; the params
    // come from the wire fixture so the announcement is the real thing.
    let fixture: serde_json::Value =
        serde_json::from_str(REGISTER_CAPABILITY_FIXTURE).expect("the fixture is valid JSON");
    let registration_params = fixture["params"].clone();
    let server = Server::builder(())
        .request::<Trigger, _, _>(
            move |_state: Arc<()>,
                  ctx: ServerContext,
                  params: serde_json::Value,
                  _ct: CancellationToken| {
                async move {
                    ctx.client()
                        .register_capability(serde_json::from_value(params).unwrap())
                        .await
                        .expect("the client accepted the registration");
                    Ok("registered".to_string())
                }
            },
        )
        .build()
        .expect("the outgoing-request server builds");
    let mut session = start(server).await;
    init(&mut session).await;

    session
        .in_tx
        .send(inbound_request(2, Trigger::METHOD, registration_params))
        .unwrap();
    let outbound = receive(&mut session.out_rx).await;
    assert_wire_request(&outbound, REGISTER_CAPABILITY_FIXTURE);
    let id = outbound_number_id(&outbound);
    session
        .in_tx
        .send(success_response(id, json!(null)))
        .unwrap();
    let response = receive(&mut session.out_rx).await;
    assert_eq!(response.id(), Some(&RequestId::Number(2)));
    assert_eq!(result_string(&response), "registered");

    // The Router never gained a hover route: dispatch still answers
    // MethodNotFound for the dynamically registered method.
    session
        .in_tx
        .send(inbound_request(
            3,
            "textDocument/hover",
            json!({
                "textDocument": { "uri": "file:///main.rs" },
                "position": { "line": 0, "character": 0 },
            }),
        ))
        .unwrap();
    let response = receive(&mut session.out_rx).await;
    match response {
        RawMessage::Response {
            id: RequestId::Number(3),
            result: Err(e),
        } => assert_eq!(
            e.code, -32601,
            "dynamic registration must not add a local route"
        ),
        other => panic!("expected a MethodNotFound error for id 3, got {other:?}"),
    }
    finish(session).await;
}
