//! End-to-end coverage for the pull-diagnostics capability family (issue #72).

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use tokio::sync::mpsc;

use lspf::types::{
    DiagnosticOptions, DiagnosticServerCapabilities, DocumentDiagnosticParams,
    DocumentDiagnosticReport, DocumentDiagnosticReportResult, FullDocumentDiagnosticReport,
    RelatedFullDocumentDiagnosticReport, WorkspaceDiagnosticParams, WorkspaceDiagnosticReport,
    WorkspaceDiagnosticReportResult,
};
use lspf::{
    BuildError, CancellationToken, LspError, RawMessage, RequestId, Server, ServerContext,
    Transport, TransportError, TransportReader, TransportWriter,
};

struct AppState;

async fn document_diagnostic(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    params: DocumentDiagnosticParams,
    _ct: CancellationToken,
) -> Result<DocumentDiagnosticReportResult, LspError> {
    Ok(
        DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: params.identifier,
                items: Vec::new(),
            },
        })
        .into(),
    )
}

async fn workspace_diagnostic(
    _state: Arc<AppState>,
    _ctx: ServerContext,
    params: WorkspaceDiagnosticParams,
    _ct: CancellationToken,
) -> Result<WorkspaceDiagnosticReportResult, LspError> {
    assert_eq!(params.identifier.as_deref(), Some("compiler"));
    Ok(WorkspaceDiagnosticReport::default().into())
}

fn options(identifier: &str, workspace_diagnostics: bool) -> DiagnosticOptions {
    DiagnosticOptions {
        identifier: Some(identifier.to_string()),
        inter_file_dependencies: true,
        workspace_diagnostics,
        work_done_progress_options: Default::default(),
    }
}

struct ChannelTransport {
    in_rx: mpsc::UnboundedReceiver<RawMessage>,
    out_tx: mpsc::UnboundedSender<RawMessage>,
}

struct ChannelReader {
    in_rx: mpsc::UnboundedReceiver<RawMessage>,
}

struct ChannelWriter {
    out_tx: mpsc::UnboundedSender<RawMessage>,
}

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (
            ChannelReader { in_rx: self.in_rx },
            ChannelWriter {
                out_tx: self.out_tx,
            },
        )
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.in_rx.recv().await.ok_or(TransportError::Closed)
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, msg: RawMessage) -> Result<(), TransportError> {
        self.out_tx.send(msg).map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

fn request(id: i32, method: &'static str, params: serde_json::Value) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn initialize_request(id: i32) -> RawMessage {
    request(
        id,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    )
}

fn exit() -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed("exit"),
        params: Bytes::from_static(b"null"),
    }
}

async fn drive(server: Server<AppState>, messages: Vec<RawMessage>) -> Vec<RawMessage> {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let transport = ChannelTransport { in_rx, out_tx };
    let mut handle = tokio::spawn(async move { server.serve(transport).await });
    let mut server_done = false;
    let mut outbox = Vec::new();

    'messages: for msg in messages {
        let response_id = msg.id().cloned();
        if in_tx.send(msg).is_err() {
            break;
        }
        if let Some(response_id) = response_id {
            tokio::select! {
                response = out_rx.recv() => {
                    if let Some(response) = response {
                        assert_eq!(response.id(), Some(&response_id));
                        outbox.push(response);
                    } else {
                        (&mut handle)
                            .await
                            .expect("server task did not panic")
                            .expect("serve ended cleanly");
                        server_done = true;
                        break 'messages;
                    }
                }
                result = &mut handle => {
                    result
                        .expect("server task did not panic")
                        .expect("serve ended cleanly");
                    server_done = true;
                    break 'messages;
                }
            }
        }
    }
    drop(in_tx);

    if !server_done {
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("serve returned within 2s")
            .expect("server task did not panic")
            .expect("serve ended cleanly");
    }
    outbox.extend(std::iter::from_fn(|| out_rx.try_recv().ok()));
    outbox
}

fn response(outbox: &[RawMessage], id: i32) -> &RawMessage {
    outbox
        .iter()
        .find(|message| {
            matches!(message, RawMessage::Response { id: response_id, .. }
                if *response_id == RequestId::Number(id))
        })
        .expect("response exists")
}

fn ok_result(outbox: &[RawMessage], id: i32) -> serde_json::Value {
    match response(outbox, id) {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => serde_json::from_slice(bytes).unwrap(),
        other => panic!("expected success response, got {other:?}"),
    }
}

fn error_code(outbox: &[RawMessage], id: i32) -> Option<i32> {
    match response(outbox, id) {
        RawMessage::Response {
            result: Err(error), ..
        } => Some(error.code),
        _ => None,
    }
}

fn diagnostic_provider(outbox: &[RawMessage]) -> DiagnosticServerCapabilities {
    let init: lspf::types::InitializeResult = serde_json::from_value(ok_result(outbox, 1)).unwrap();
    init.capabilities
        .diagnostic_provider
        .expect("a diagnostic route contributes diagnosticProvider")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn either_route_contributes_a_provider_and_combined_order_is_stable() {
    let document_options = options("compiler", false);
    let combined_options = options("compiler", true);
    let document_only = Server::builder(AppState)
        .feature(
            lspf::features::document_diagnostic(document_options.clone()),
            document_diagnostic,
        )
        .build()
        .unwrap();
    let workspace_only = Server::builder(AppState)
        .feature(
            lspf::features::workspace_diagnostic(combined_options.clone()),
            workspace_diagnostic,
        )
        .build()
        .unwrap();
    let document_first = Server::builder(AppState)
        .feature(
            lspf::features::document_diagnostic(combined_options.clone()),
            document_diagnostic,
        )
        .feature(
            lspf::features::workspace_diagnostic(combined_options.clone()),
            workspace_diagnostic,
        )
        .build()
        .unwrap();
    let workspace_first = Server::builder(AppState)
        .feature(
            lspf::features::workspace_diagnostic(combined_options.clone()),
            workspace_diagnostic,
        )
        .feature(
            lspf::features::document_diagnostic(combined_options.clone()),
            document_diagnostic,
        )
        .build()
        .unwrap();

    let document_only = drive(document_only, vec![initialize_request(1), exit()]).await;
    let workspace_only = drive(workspace_only, vec![initialize_request(1), exit()]).await;
    let document_first = drive(document_first, vec![initialize_request(1), exit()]).await;
    let workspace_first = drive(workspace_first, vec![initialize_request(1), exit()]).await;
    let document_expected = DiagnosticServerCapabilities::Options(document_options);
    let combined_expected = DiagnosticServerCapabilities::Options(combined_options);

    assert_eq!(diagnostic_provider(&document_only), document_expected);
    assert_eq!(diagnostic_provider(&workspace_only), combined_expected);
    assert_eq!(diagnostic_provider(&document_first), combined_expected);
    assert_eq!(diagnostic_provider(&workspace_first), combined_expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnostic_routes_dispatch_typed_values_and_share_one_capability() {
    let options = options("compiler", true);
    let server = Server::builder(AppState)
        .feature(
            lspf::features::document_diagnostic(options.clone()),
            document_diagnostic,
        )
        .feature(
            lspf::features::workspace_diagnostic(options.clone()),
            workspace_diagnostic,
        )
        .build()
        .expect("compatible diagnostics build");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            request(
                2,
                "textDocument/diagnostic",
                json!({
                    "textDocument": { "uri": "file:///a.rs" },
                    "identifier": "document-result",
                    "previousResultId": null
                }),
            ),
            request(
                3,
                "workspace/diagnostic",
                json!({ "identifier": "compiler", "previousResultIds": [] }),
            ),
            exit(),
        ],
    )
    .await;

    let init: lspf::types::InitializeResult =
        serde_json::from_value(ok_result(&outbox, 1)).unwrap();
    assert_eq!(
        init.capabilities.diagnostic_provider,
        Some(DiagnosticServerCapabilities::Options(options))
    );
    let document: DocumentDiagnosticReportResult =
        serde_json::from_value(ok_result(&outbox, 2)).unwrap();
    assert!(matches!(
        document,
        DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(_))
    ));
    let workspace: WorkspaceDiagnosticReportResult =
        serde_json::from_value(ok_result(&outbox, 3)).unwrap();
    assert_eq!(
        workspace,
        WorkspaceDiagnosticReportResult::Report(WorkspaceDiagnosticReport::default())
    );
}

#[test]
fn static_diagnostic_option_drift_is_a_capability_conflict() {
    let result = Server::builder(AppState)
        .feature(
            lspf::features::document_diagnostic(options("compiler", true)),
            document_diagnostic,
        )
        .feature(
            lspf::features::workspace_diagnostic(options("linter", true)),
            workspace_diagnostic,
        )
        .build();
    let error = match result {
        Ok(_) => panic!("different identifiers must conflict"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        BuildError::ConflictingCapability {
            field: "diagnosticProvider"
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_diagnostic_option_drift_uses_the_same_validation() {
    let server = Server::builder(AppState)
        .feature(
            lspf::features::document_diagnostic(options("compiler", false)),
            document_diagnostic,
        )
        .configure_initialize(|_params, registrar| {
            registrar.feature(
                lspf::features::workspace_diagnostic(options("compiler", true)),
                workspace_diagnostic,
            );
            Ok(())
        })
        .build()
        .expect("conditional registrations run during initialize");

    let outbox = drive(server, vec![initialize_request(1)]).await;
    assert_eq!(error_code(&outbox, 1), Some(-32603));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compatible_static_and_conditional_routes_merge() {
    let shared_options = options("compiler", true);
    let server = Server::builder(AppState)
        .feature(
            lspf::features::document_diagnostic(shared_options.clone()),
            document_diagnostic,
        )
        .configure_initialize(move |_params, registrar| {
            registrar.feature(
                lspf::features::workspace_diagnostic(shared_options.clone()),
                workspace_diagnostic,
            );
            Ok(())
        })
        .build()
        .expect("static options remain available to the initialize transaction");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;
    assert_eq!(
        diagnostic_provider(&outbox),
        DiagnosticServerCapabilities::Options(options("compiler", true))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn combined_diagnostics_capability_is_byte_stable() {
    let options = options("compiler", true);
    let server = Server::builder(AppState)
        .feature(
            lspf::features::document_diagnostic(options.clone()),
            document_diagnostic,
        )
        .feature(
            lspf::features::workspace_diagnostic(options),
            workspace_diagnostic,
        )
        .build()
        .expect("compatible diagnostics build");

    let outbox = drive(server, vec![initialize_request(1), exit()]).await;
    let wire = match response(&outbox, 1) {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => String::from_utf8(bytes.to_vec()).unwrap(),
        other => panic!("expected initialize response, got {other:?}"),
    };
    let fixture = include_str!("fixtures/diagnostic_provider_combined.json").trim_end();
    assert!(
        wire.contains(&format!("\"diagnosticProvider\":{fixture}")),
        "the combined diagnosticProvider must stay byte-stable; wire: {wire}"
    );
}
