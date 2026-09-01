//! The template language server: a built `Server` served over stdio.
//!
//! It demonstrates the complete typed journey the framework supports with no
//! handwritten `ServerCapabilities` field and no framework modification — the
//! registrations are the capabilities:
//!
//! - standard features registered through sealed descriptors — hover,
//!   completion, and the dependent completion resolve;
//! - two typed Commands dispatched beneath `workspace/executeCommand`, one of
//!   which reads multi-root workspace state and one of which reads a file
//!   that is not open in the editor;
//! - a post-mutation hook for `textDocument/didOpen`, observing the document
//!   the framework has already opened;
//! - a Command running the complete outgoing-helper journey — configuration
//!   lookup, workspace edit, a custom outgoing request, dynamic registration,
//!   every stable workspace refresh, and work-done progress — plus a second
//!   Command demonstrating client-cancellable progress, all through typed
//!   helpers with no handwritten JSON;
//! - an `OsFileProvider` configured on the builder, so unopened `file:` URIs
//!   resolve from disk.
//!
//! Fork it as the starting point for a real server.

use std::str::FromStr;
use std::sync::Arc;

use lspf::types::{
    ApplyWorkspaceEditParams, CompletionItem, CompletionItemKind, CompletionOptions,
    CompletionParams, CompletionResponse, ConfigurationItem, ConfigurationParams, Contents,
    Diagnostic, DiagnosticSeverity, DidOpenTextDocumentNotification as DidOpenTextDocument,
    DidOpenTextDocumentParams, Hover, HoverParams, MarkupContent, MarkupKind, MessageType,
    Position, PublishDiagnosticsNotification as PublishDiagnostics, PublishDiagnosticsParams,
    Range, Registration, RegistrationParams, ShowMessageParams, TextEdit, Uri, WorkspaceEdit,
};
use lspf::{CancellationToken, LspError, OsFileProvider, ProgressOptions, Server, ServerContext};
use tracing::{debug, warn};

/// This server's own application state, shared by every handler as `Arc<State>`.
///
/// The framework's documents, workspace, and client are reached through the
/// `ServerContext` parameter, never stored here; a real server would keep its
/// analysis results, caches, or configuration in this struct instead. This one
/// has none, so it is empty.
struct State;

impl State {
    fn new() -> Self {
        Self
    }
}

/// The post-mutation hook for the built-in `textDocument/didOpen`.
///
/// The protocol engine has already decoded the notification and opened the
/// document by the time this runs, so the hook observes the retained
/// [`Document`](lspf::Document) through `ctx.documents()` rather than trusting
/// the wire parameters, and reports on the state every later handler will see.
async fn on_did_open(_state: Arc<State>, ctx: ServerContext, params: DidOpenTextDocumentParams) {
    let uri = params.text_document.uri;
    let Some(document) = ctx.documents().get(&uri) else {
        // The built-in mutation runs before the hook, so a missing document
        // means the notification never reached it — there is nothing to report.
        warn!(?uri, "didOpen hook found no open document");
        return;
    };
    debug!(uri = %uri.as_str(), "publishing the open diagnostic");

    let diagnostics = PublishDiagnosticsParams {
        uri,
        // The retained version, not the one on the wire: that is the revision
        // this diagnostic actually describes.
        version: document.version(),
        diagnostics: vec![Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 0,
                },
            },
            severity: Some(DiagnosticSeverity::Information),
            source: Some("lspf-hello".into()),
            message: "lspf saw this document open".into(),
            ..Diagnostic::default()
        }],
    };
    // A notification is fire-and-forget: it is encoded and enqueued
    // synchronously, and a closing connection is the only way it fails.
    if let Err(error) = ctx.client().notify::<PublishDiagnostics>(diagnostics) {
        warn!(%error, "publishing the open diagnostic failed");
    }
}

/// The typed `textDocument/hover` feature: registered through the sealed
/// [`hover`](lspf::features::hover) descriptor, which contributes
/// `hoverProvider: true` to the generated capabilities and fixes this
/// handler's parameter and result types.
///
/// The handler reads the same framework-owned document every other handler
/// sees — the engine applied `didOpen` and any incremental `didChange` before
/// this request was dispatched.
async fn hover(
    _state: Arc<State>,
    ctx: ServerContext,
    params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let uri = &params.text_document_position_params.text_document.uri;
    let Some(document) = ctx.documents().get(uri) else {
        return Ok(None);
    };
    let words = document.text().split_whitespace().count();
    let version = document
        .version()
        .map_or_else(|| "unknown".to_owned(), |version| version.to_string());
    Ok(Some(Hover {
        contents: Contents::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!(
                "`{}` · {words} words · version {version}",
                document.language_id(),
            ),
        }),
        range: None,
    }))
}

/// The typed `textDocument/completion` feature: the
/// [`completion(options)`](lspf::features::completion) descriptor advertises
/// exactly the supplied options as `completionProvider`.
async fn completion(
    _state: Arc<State>,
    _ctx: ServerContext,
    _params: CompletionParams,
    _ct: CancellationToken,
) -> Result<Option<CompletionResponse>, LspError> {
    Ok(Some(CompletionResponse::CompletionItemList(vec![
        CompletionItem {
            label: "lspf-hello".into(),
            kind: Some(CompletionItemKind::Text),
            detail: Some("a template server".into()),
            ..CompletionItem::default()
        },
        CompletionItem {
            label: "workspaceRoots".into(),
            kind: Some(CompletionItemKind::Method),
            detail: Some("read the multi-root workspace state".into()),
            ..CompletionItem::default()
        },
    ])))
}

/// The dependent `completionItem/resolve` feature. Registering it alongside
/// [`completion`] is what turns the advertised completion provider into an
/// options object carrying `resolveProvider: true`; registering it alone
/// would fail the build with a dangling `resolveProvider`.
async fn resolve_completion(
    _state: Arc<State>,
    _ctx: ServerContext,
    item: CompletionItem,
    _ct: CancellationToken,
) -> Result<CompletionItem, LspError> {
    let mut resolved = item;
    if resolved.detail.is_none() {
        resolved.detail = Some("resolved by lspf-hello".into());
    }
    Ok(resolved)
}

/// A typed Command reading the connection's live multi-root workspace state.
///
/// The engine routes `workspace/executeCommand` here by name, and the
/// registration contributes `lspf-hello.workspaceRoots` to the generated
/// `executeCommandProvider` (in registration order, ADR 0022).
async fn workspace_roots(
    _state: Arc<State>,
    ctx: ServerContext,
    _args: Vec<String>,
    _ct: CancellationToken,
) -> Result<Vec<(String, String)>, LspError> {
    Ok(ctx
        .workspace()
        .roots()
        .into_iter()
        .map(|folder| (folder.uri.as_str().to_string(), folder.name))
        .collect())
}

/// A typed Command reading a file that is not open in the editor.
///
/// `ctx.workspace().text_document` prefers editor-open text and falls back to
/// the connection's configured [`FileProvider`](lspf::FileProvider) — here the
/// `OsFileProvider` configured on the builder — so a URI the server has never
/// seen still resolves.
async fn read_file(
    _state: Arc<State>,
    ctx: ServerContext,
    args: Vec<String>,
    _ct: CancellationToken,
) -> Result<String, LspError> {
    let Some(arg) = args.into_iter().next() else {
        return Err(LspError::invalid_params(
            "lspf-hello.readFile expects one file URI argument",
        ));
    };
    let uri = Uri::from_str(&arg)
        .map_err(|error| LspError::invalid_params(format!("invalid URI: {error}")))?;
    let document =
        ctx.workspace().text_document(&uri).await.map_err(|error| {
            LspError::invalid_request(format!("cannot read `{uri:?}`: {error}"))
        })?;
    Ok(document.text())
}

/// The custom outgoing request the journey sends to the client. A marker type
/// implementing the re-exported [`lspf::types::Request`] trait is the
/// whole mechanism: the framework allocates the ID, correlates the response,
/// and cancels on the wire if the future is dropped.
enum HelloPing {}

impl lspf::types::Request for HelloPing {
    type Params = String;
    type Result = String;
    const METHOD: lspf::types::LspRequestMethod<'static> =
        lspf::types::LspRequestMethod::Custom("lspf-hello/ping");
    const MESSAGE_DIRECTION: lspf::types::MessageDirection =
        lspf::types::MessageDirection::ServerToClient;
}

/// One journey step's name and outcome, reported back as the Command result so
/// the caller can see how every helper call ended.
type Step = (String, String);

/// Record a step's outcome: `Ok(_)` summaries on success, the error text on
/// failure. The journey keeps going after a failure — a helper that returns
/// `Err` has already cleaned up after itself.
fn record(steps: &mut Vec<Step>, name: &str, outcome: Result<String, lspf::ClientError>) {
    match outcome {
        Ok(summary) => steps.push((name.to_string(), summary)),
        Err(error) => {
            warn!(%error, step = name, "outgoing journey step failed");
            steps.push((name.to_string(), format!("error: {error}")));
        }
    }
}

/// A typed Command running the complete outgoing-helper journey against the
/// real client — every step goes through a named [`ClientHandle`](lspf::ClientHandle)
/// helper with native LSP types and no handwritten JSON.
///
/// In wire order: a `workspace/configuration` lookup whose result is reported
/// with `window/showMessage`, a `workspace/applyEdit` against the document URI
/// passed as the first argument, the custom `lspf-hello/ping` request, a
/// dynamic `client/registerCapability` announcement, all five stable workspace
/// refreshes, and a cancellable work-done progress lifecycle. Each step's
/// outcome is collected into the returned list, so a client that answers one
/// request with an error watches the rest of the journey complete regardless.
async fn outgoing_journey(
    _state: Arc<State>,
    ctx: ServerContext,
    args: Vec<String>,
    _ct: CancellationToken,
) -> Result<Vec<Step>, LspError> {
    let Some(target) = args.into_iter().next() else {
        return Err(LspError::invalid_params(
            "lspf-hello.outgoingJourney expects the target document URI",
        ));
    };
    let uri = Uri::from_str(&target)
        .map_err(|error| LspError::invalid_params(format!("invalid URI: {error}")))?;
    let client = ctx.client();
    let mut steps: Vec<Step> = Vec::new();

    // 1. Configuration lookup: the items go out verbatim and the result comes
    //    back to this caller only — the Workspace snapshot is not updated.
    let configuration = client
        .configuration(ConfigurationParams {
            items: vec![ConfigurationItem {
                scope_uri: None,
                section: Some("lspf-hello".into()),
            }],
        })
        .await
        .map(|values| values.into_iter().next().unwrap_or_default().to_string());
    let configured_text = configuration
        .as_ref()
        .map_or_else(|error| format!("unavailable ({error})"), Clone::clone);
    record(&mut steps, "configuration", configuration);

    // 2. Messaging: a fire-and-forget `window/showMessage` reports the
    //    outcome. It is encoded and enqueued synchronously.
    if let Err(error) = client.show_message(ShowMessageParams {
        kind: MessageType::Info,
        message: format!("lspf-hello configuration: {configured_text}"),
    }) {
        warn!(%error, "showing the configuration message failed");
    }

    // 3. Workspace edit: the edit is sent exactly as built; the client's
    //    applied flag and failure reason come back verbatim.
    let applied = client
        .apply_edit(ApplyWorkspaceEditParams {
            label: Some("lspf-hello touch".into()),
            metadata: None,
            edit: WorkspaceEdit {
                changes: Some(
                    [(
                        uri,
                        vec![TextEdit {
                            range: Range {
                                start: Position {
                                    line: 0,
                                    character: 0,
                                },
                                end: Position {
                                    line: 0,
                                    character: 0,
                                },
                            },
                            new_text: "// touched by lspf-hello\n".into(),
                        }],
                    )]
                    .into_iter()
                    .collect(),
                ),
                ..WorkspaceEdit::default()
            },
        })
        .await
        .map(|response| match response.failure_reason {
            Some(reason) => format!("applied:{} ({reason})", response.applied),
            None => format!("applied:{}", response.applied),
        });
    record(&mut steps, "applyEdit", applied);

    // 4. The custom outgoing request: a user-defined method with typed params
    //    and result, sent through the same broker as every named helper.
    let pong = client.request::<HelloPing>("ping".to_string()).await;
    record(&mut steps, "ping", pong);

    // 5. Dynamic registration: the announcement tells the client about the
    //    change; the frozen Router and the initialize capabilities stay
    //    untouched, and the framework keeps no registration state.
    let registered = client
        .register_capability(RegistrationParams {
            registrations: vec![Registration {
                id: "lspf-hello.watch".into(),
                method: "workspace/didChangeWatchedFiles".into(),
                register_options: None,
            }],
        })
        .await
        .map(|()| "registered".to_string());
    record(&mut steps, "registerCapability", registered);

    // 6. Every stable workspace refresh: no parameters, a `null`
    //    acknowledgement, and no recomputation policy owned by the helper.
    let refreshes = [
        ("codeLens", client.refresh_code_lenses().await),
        ("diagnostic", client.refresh_diagnostics().await),
        ("inlayHint", client.refresh_inlay_hints().await),
        ("inlineValue", client.refresh_inline_values().await),
        ("semanticTokens", client.refresh_semantic_tokens().await),
    ];
    for (name, result) in refreshes {
        record(&mut steps, name, result.map(|()| "refreshed".to_string()));
    }

    // 7. Cancellable work-done progress: create completes before the token
    //    registers, one begin carries the options verbatim, and `end`
    //    consumes the handle and removes the token either way.
    let progress = match client
        .begin_progress(
            ProgressOptions::new("Outgoing journey")
                .cancellable(true)
                .message("wrapping up")
                .percentage(0),
        )
        .await
    {
        Ok(handle) => {
            let reported = handle.report(Some("halfway".into()), Some(50));
            let ended = reported.and_then(|()| handle.end(Some("done".into())));
            ended.map(|()| "completed".to_string())
        }
        Err(error) => Err(error),
    };
    record(&mut steps, "progress", progress);

    Ok(steps)
}

/// A typed Command demonstrating client-cancellable work-done progress.
///
/// The client cancels through `window/workDoneProgress/cancel`; the framework
/// fires the handle's [`CancellationToken`] and sends nothing by itself — the
/// application observes the cancellation and ends the progress. A client that
/// never cancels leaves this Command pending, which is the honest shape of
/// work that only ends when it is cancelled or finished.
async fn cancellable_progress(
    _state: Arc<State>,
    ctx: ServerContext,
    _args: Vec<String>,
    _ct: CancellationToken,
) -> Result<Vec<Step>, LspError> {
    let handle = ctx
        .client()
        .begin_progress(ProgressOptions::new("Cancellable demo").cancellable(true))
        .await
        .map_err(LspError::internal)?;
    handle.report(None, Some(0)).map_err(LspError::internal)?;
    handle.cancellation_token().cancelled().await;
    handle
        .end(Some("cancelled".into()))
        .map_err(LspError::internal)?;
    Ok(vec![("outcome".to_string(), "cancelled".to_string())])
}

fn completion_options() -> CompletionOptions {
    CompletionOptions {
        trigger_characters: Some(vec![".".to_string()]),
        ..CompletionOptions::default()
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::from_default_env();
    if std::env::var("LSPF_LOG_FORMAT").as_deref() == Ok("json") {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(filter)
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(false)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            // Editor output channels display ANSI bytes as visible symbols.
            .with_ansi(false)
            .with_env_filter(filter)
            .init();
    }
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    // Logs go to stderr: stdout carries the LSP wire protocol and nothing else.
    init_tracing();

    let server = Server::builder(State::new())
        // Unopened `file:` URIs resolve from the local filesystem.
        .file_provider(OsFileProvider::new())
        // Standard features: the registrations are the capability catalog.
        .feature(lspf::features::hover(), hover)
        .feature(lspf::features::completion(completion_options()), completion)
        .feature(lspf::features::completion_resolve(), resolve_completion)
        // Typed Commands beneath `workspace/executeCommand`.
        .command("lspf-hello.workspaceRoots", workspace_roots)
        .command("lspf-hello.readFile", read_file)
        .command("lspf-hello.outgoingJourney", outgoing_journey)
        .command("lspf-hello.cancellableProgress", cancellable_progress)
        // The post-mutation hook observes the framework's document sync.
        .notification::<DidOpenTextDocument, _, _>(on_did_open)
        .build()
        .expect("the static registrations are valid");
    // Serving reports how the connection ended and never terminates the
    // process; turning that Outcome into a process disposition is this
    // binary's decision.
    let outcome = lspf::stdio(server).serve().await?;
    std::process::exit(outcome.code());
}
