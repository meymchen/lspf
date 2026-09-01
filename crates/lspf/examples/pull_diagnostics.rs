//! Pull-model document and workspace diagnostics server.

mod example_support;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use lspf::types::notification::{DidChangeTextDocument, DidOpenTextDocument};
use lspf::types::{
    Diagnostic, DiagnosticOptions, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
    DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    FullDocumentDiagnosticReport, RelatedFullDocumentDiagnosticReport,
    RelatedUnchangedDocumentDiagnosticReport, UnchangedDocumentDiagnosticReport, Uri,
    WorkspaceDiagnosticParams, WorkspaceDiagnosticReport, WorkspaceDiagnosticReportResult,
    WorkspaceDocumentDiagnosticReport, WorkspaceFullDocumentDiagnosticReport,
    WorkspaceUnchangedDocumentDiagnosticReport,
};
use lspf::{CancellationToken, LspError, Server, ServerContext};

type Report = (Option<i32>, Vec<Diagnostic>);

#[derive(Default)]
struct State {
    reports: Mutex<HashMap<Uri, Report>>,
}

fn update(state: &State, ctx: &ServerContext, uri: &Uri) {
    let Some(document) = ctx.documents().get(uri) else {
        return;
    };
    state.reports.lock().unwrap().insert(
        uri.clone(),
        (
            document.version(),
            example_support::sum_diagnostics(&document.text()),
        ),
    );
}

async fn did_open(state: Arc<State>, ctx: ServerContext, params: DidOpenTextDocumentParams) {
    update(&state, &ctx, &params.text_document.uri);
}

async fn did_change(state: Arc<State>, ctx: ServerContext, params: DidChangeTextDocumentParams) {
    update(
        &state,
        &ctx,
        &params.text_document.text_document_identifier.uri,
    );
}

fn result_id(uri: &Uri, version: Option<i32>) -> String {
    format!("{}@{}", uri.as_str(), version.unwrap_or_default())
}

async fn document_diagnostic(
    state: Arc<State>,
    ctx: ServerContext,
    params: DocumentDiagnosticParams,
    _: CancellationToken,
) -> Result<DocumentDiagnosticReportResult, LspError> {
    update(&state, &ctx, &params.text_document.uri);
    let reports = state.reports.lock().unwrap();
    let (version, diagnostics) = reports
        .get(&params.text_document.uri)
        .cloned()
        .unwrap_or_default();
    let id = result_id(&params.text_document.uri, version);
    let report = if params.previous_result_id.as_deref() == Some(&id) {
        DocumentDiagnosticReport::RelatedUnchangedDocumentDiagnosticReport(
            RelatedUnchangedDocumentDiagnosticReport {
                related_documents: None,
                unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport {
                    result_id: id,
                },
            },
        )
    } else {
        DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(
            RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: Some(id),
                    items: diagnostics,
                },
            },
        )
    };
    Ok(report.into())
}

async fn workspace_diagnostic(
    state: Arc<State>,
    _: ServerContext,
    params: WorkspaceDiagnosticParams,
    _: CancellationToken,
) -> Result<WorkspaceDiagnosticReportResult, LspError> {
    let previous: HashSet<_> = params
        .previous_result_ids
        .into_iter()
        .map(|id| id.value)
        .collect();
    let reports = state.reports.lock().unwrap();
    let items = reports
        .iter()
        .map(|(uri, (version, diagnostics))| {
            let id = result_id(uri, *version);
            if previous.contains(&id) {
                WorkspaceDocumentDiagnosticReport::WorkspaceUnchangedDocumentDiagnosticReport(
                    WorkspaceUnchangedDocumentDiagnosticReport {
                        uri: uri.clone(),
                        version: *version,
                        unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport {
                            result_id: id,
                        },
                    },
                )
            } else {
                WorkspaceDocumentDiagnosticReport::WorkspaceFullDocumentDiagnosticReport(
                    WorkspaceFullDocumentDiagnosticReport {
                        uri: uri.clone(),
                        version: *version,
                        full_document_diagnostic_report: FullDocumentDiagnosticReport {
                            result_id: Some(id),
                            items: diagnostics.clone(),
                        },
                    },
                )
            }
        })
        .collect();
    Ok(WorkspaceDiagnosticReport { items }.into())
}

fn options() -> DiagnosticOptions {
    DiagnosticOptions {
        identifier: Some("pull-diagnostics".to_string()),
        inter_file_dependencies: false,
        workspace_diagnostics: true,
        work_done_progress_options: Default::default(),
    }
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State::default())
        .notification::<DidOpenTextDocument, _, _>(did_open)
        .notification::<DidChangeTextDocument, _, _>(did_change)
        .feature(
            lspf::features::document_diagnostic(options()),
            document_diagnostic,
        )
        .feature(
            lspf::features::workspace_diagnostic(options()),
            workspace_diagnostic,
        )
        .build()
        .expect("pull-diagnostic registrations are valid");
    example_support::serve(server).await
}
