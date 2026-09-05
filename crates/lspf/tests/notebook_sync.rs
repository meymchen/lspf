//! End-to-end coverage for notebook synchronization (issue #251, ADR 0034).
//!
//! The four `notebookDocument/*` notifications are protocol built-ins: the
//! engine decodes them, mutates the connection-owned notebook layer and
//! [`Documents`] serially, and only then does a user registration run as a
//! post-mutation hook. These tests drive a connection-owned [`Server`] over an
//! in-memory transport and prove the wire journey a real editor produces —
//! open, insert a cell, edit a cell's text, delete a cell, close — leaves both
//! stores agreeing at every step, that closing releases every cell text
//! Document,
//! and that a splice the peer sends out of range is a protocol error rather
//! than a panic.

use std::borrow::Cow;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use lspf::types::notification::{
    DidChangeNotebookDocument, DidChangeTextDocument, DidCloseNotebookDocument,
    DidCloseTextDocument, DidOpenNotebookDocument, DidOpenTextDocument, DidSaveNotebookDocument,
};
use lspf::types::request::Request;
use lspf::types::{
    DidChangeNotebookDocumentParams, DidCloseNotebookDocumentParams, DidOpenNotebookDocumentParams,
    DidSaveNotebookDocumentParams, NotebookDocumentFilterWithNotebook, NotebookDocumentSyncOptions,
    Uri,
};
use lspf::{
    CancellationToken, ConnectionDirection, ConnectionFailure, ConnectionFailureCategory, LspError,
    RawMessage, RequestId, ResourcePolicy, Server, ServerContext, Transport, TransportError,
    TransportReader, TransportWriter,
};

const NOTEBOOK: &str = "file:///analysis.ipynb";
const CELL_ONE: &str = "file:///analysis.ipynb#c1";
const CELL_TWO: &str = "file:///analysis.ipynb#c2";
const CELL_THREE: &str = "file:///analysis.ipynb#c3";

// --- What each hook observed -------------------------------------------------

/// One hook invocation, recorded as the state the hook saw *after* the built-in
/// mutation ran. Every field comes from `ctx.notebooks()` or `ctx.documents()`,
/// so the record is exactly what a user handler can observe.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Open {
        cells: Vec<String>,
        version: i32,
    },
    Change {
        cells: Vec<String>,
        version: i32,
    },
    Save {
        still_present: bool,
    },
    Close {
        still_present: bool,
        cell_texts: Vec<Option<String>>,
    },
    /// A `textDocument/*` hook fired. A notebook notification must never
    /// produce one (ADR 0034), so any entry here is a failure.
    TextDocument(&'static str),
}

type Log = Arc<Mutex<Vec<Seen>>>;

struct AppState {
    seen: Log,
}

fn uri(spelling: &str) -> Uri {
    Uri::from_str(spelling).expect("test URIs are valid")
}

fn cell_uris(notebook: &lspf::Notebook) -> Vec<String> {
    notebook
        .cells()
        .iter()
        .map(|cell| cell.document.as_str().to_string())
        .collect()
}

async fn on_did_open(
    state: Arc<AppState>,
    ctx: ServerContext,
    params: DidOpenNotebookDocumentParams,
) {
    let notebook = ctx
        .notebooks()
        .get(&params.notebook_document.uri)
        .expect("the built-in registered the notebook before its hook");
    state.seen.lock().unwrap().push(Seen::Open {
        cells: cell_uris(&notebook),
        version: notebook.version(),
    });
}

async fn on_did_change(
    state: Arc<AppState>,
    ctx: ServerContext,
    params: DidChangeNotebookDocumentParams,
) {
    let notebook = ctx
        .notebooks()
        .get(&params.notebook_document.uri)
        .expect("the built-in applied the change before its hook");
    state.seen.lock().unwrap().push(Seen::Change {
        cells: cell_uris(&notebook),
        version: notebook.version(),
    });
}

async fn on_did_save(
    state: Arc<AppState>,
    ctx: ServerContext,
    params: DidSaveNotebookDocumentParams,
) {
    let still_present = ctx.notebooks().get(&params.notebook_document.uri).is_some();
    state
        .seen
        .lock()
        .unwrap()
        .push(Seen::Save { still_present });
}

async fn on_did_close(
    state: Arc<AppState>,
    ctx: ServerContext,
    params: DidCloseNotebookDocumentParams,
) {
    let still_present = ctx.notebooks().get(&params.notebook_document.uri).is_some();
    let cell_texts = [CELL_ONE, CELL_TWO, CELL_THREE]
        .iter()
        .map(|spelling| {
            ctx.documents()
                .get(&uri(spelling))
                .map(|document| document.text())
        })
        .collect();
    state.seen.lock().unwrap().push(Seen::Close {
        still_present,
        cell_texts,
    });
}

// --- A custom request that reads both stores through their views -------------

#[derive(Debug, Serialize, Deserialize)]
struct ProbeParams {
    notebook: String,
    /// Cell URIs to look up in the document store, in order.
    cells: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct NotebookProbe {
    notebook_type: String,
    version: i32,
    metadata: Option<Value>,
    cells: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProbeResult {
    notebook: Option<NotebookProbe>,
    /// Each requested cell's text, or `None` when no document is tracked.
    texts: Vec<Option<String>>,
    /// Each requested cell's owning notebook, resolved through the view.
    owners: Vec<Option<String>>,
}

/// A custom request whose only job is to report what the notebook and document
/// views see, so a test can assert the state after every step of a journey.
enum Probe {}

impl Request for Probe {
    type Params = ProbeParams;
    type Result = ProbeResult;
    const METHOD: &'static str = "custom/probe";
}

async fn probe(
    _state: Arc<AppState>,
    ctx: ServerContext,
    params: ProbeParams,
    _ct: CancellationToken,
) -> Result<ProbeResult, LspError> {
    let notebooks = ctx.notebooks();
    let documents = ctx.documents();
    let notebook = notebooks
        .get(&uri(&params.notebook))
        .map(|notebook| NotebookProbe {
            notebook_type: notebook.notebook_type().to_string(),
            version: notebook.version(),
            metadata: notebook
                .metadata()
                .map(|metadata| serde_json::to_value(metadata).expect("metadata is JSON")),
            cells: cell_uris(&notebook),
        });
    Ok(ProbeResult {
        notebook,
        texts: params
            .cells
            .iter()
            .map(|spelling| documents.get(&uri(spelling)).map(|d| d.text()))
            .collect(),
        owners: params
            .cells
            .iter()
            .map(|spelling| {
                notebooks
                    .notebook_for_cell(&uri(spelling))
                    .map(|notebook| notebook.uri().as_str().to_string())
            })
            .collect(),
    })
}

// --- In-memory transport -----------------------------------------------------

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

// --- Envelope helpers --------------------------------------------------------

fn request(id: i32, method: &'static str, params: Value) -> RawMessage {
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

fn notification(method: &'static str, params: Value) -> RawMessage {
    RawMessage::Notification {
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

/// A code cell in the notebook's cell array.
fn cell(uri: &str) -> Value {
    json!({ "kind": 2, "document": uri })
}

/// A cell's text document, as the client sends it alongside the cell array.
fn cell_document(uri: &str, text: &str) -> Value {
    json!({ "uri": uri, "languageId": "python", "version": 1, "text": text })
}

fn notebook_did_open(version: i32, cells: Vec<&str>, texts: Vec<(&str, &str)>) -> RawMessage {
    notebook_did_open_at(NOTEBOOK, version, cells, texts)
}

fn notebook_did_open_at(
    notebook_uri: &str,
    version: i32,
    cells: Vec<&str>,
    texts: Vec<(&str, &str)>,
) -> RawMessage {
    notification(
        "notebookDocument/didOpen",
        json!({
            "notebookDocument": {
                "uri": notebook_uri,
                "notebookType": "jupyter-notebook",
                "version": version,
                "cells": cells.iter().map(|uri| cell(uri)).collect::<Vec<_>>(),
            },
            "cellTextDocuments": texts
                .iter()
                .map(|(uri, text)| cell_document(uri, text))
                .collect::<Vec<_>>(),
        }),
    )
}

/// A structural cell change: the splice plus the cell text Documents it opens and
/// closes.
fn notebook_structure_change(
    version: i32,
    start: u32,
    delete_count: u32,
    inserted: Vec<&str>,
    did_open: Vec<(&str, &str)>,
    did_close: Vec<&str>,
) -> RawMessage {
    notification(
        "notebookDocument/didChange",
        json!({
            "notebookDocument": { "uri": NOTEBOOK, "version": version },
            "change": {
                "cells": {
                    "structure": {
                        "array": {
                            "start": start,
                            "deleteCount": delete_count,
                            "cells": inserted.iter().map(|uri| cell(uri)).collect::<Vec<_>>(),
                        },
                        "didOpen": did_open
                            .iter()
                            .map(|(uri, text)| cell_document(uri, text))
                            .collect::<Vec<_>>(),
                        "didClose": did_close
                            .iter()
                            .map(|uri| json!({ "uri": uri }))
                            .collect::<Vec<_>>(),
                    }
                }
            }
        }),
    )
}

/// An incremental edit to one cell's text, on the same wire shape a
/// `textDocument/didChange` range edit uses.
fn notebook_text_change(
    notebook_version: i32,
    cell_uri: &str,
    cell_version: i32,
    start: u32,
    end: u32,
    text: &str,
) -> RawMessage {
    notification(
        "notebookDocument/didChange",
        json!({
            "notebookDocument": { "uri": NOTEBOOK, "version": notebook_version },
            "change": {
                "cells": {
                    "textContent": [{
                        "document": { "uri": cell_uri, "version": cell_version },
                        "changes": [{
                            "range": {
                                "start": { "line": 0, "character": start },
                                "end": { "line": 0, "character": end }
                            },
                            "text": text
                        }]
                    }]
                }
            }
        }),
    )
}

fn notebook_did_save() -> RawMessage {
    notification(
        "notebookDocument/didSave",
        json!({ "notebookDocument": { "uri": NOTEBOOK } }),
    )
}

fn notebook_did_close(cell_text_documents: Vec<&str>) -> RawMessage {
    notification(
        "notebookDocument/didClose",
        json!({
            "notebookDocument": { "uri": NOTEBOOK },
            "cellTextDocuments": cell_text_documents
                .iter()
                .map(|uri| json!({ "uri": uri }))
                .collect::<Vec<_>>(),
        }),
    )
}

fn probe_request(id: i32) -> RawMessage {
    probe_request_at(id, NOTEBOOK, vec![CELL_ONE, CELL_TWO, CELL_THREE])
}

fn probe_request_at(id: i32, notebook: &str, cells: Vec<&str>) -> RawMessage {
    request(
        id,
        "custom/probe",
        json!({
            "notebook": notebook,
            "cells": cells,
        }),
    )
}

// --- Harness -----------------------------------------------------------------

/// The selector every test server advertises. Notebook sync is opt-in, so a
/// server that omits this call receives no notebook notification at all.
fn notebook_sync_options() -> NotebookDocumentSyncOptions {
    NotebookDocumentSyncOptions::new(
        vec![NotebookDocumentFilterWithNotebook::new("jupyter-notebook".into(), None).into()],
        Some(true),
    )
}

fn observing_server(seen: &Log) -> Server<AppState> {
    observing_server_with_policy(seen, ResourcePolicy::default())
}

fn observing_server_with_policy(seen: &Log, policy: ResourcePolicy) -> Server<AppState> {
    Server::builder(AppState {
        seen: Arc::clone(seen),
    })
    .notebook_document_sync(notebook_sync_options())
    .notification::<DidOpenNotebookDocument, _, _>(on_did_open)
    .notification::<DidChangeNotebookDocument, _, _>(on_did_change)
    .notification::<DidSaveNotebookDocument, _, _>(on_did_save)
    .notification::<DidCloseNotebookDocument, _, _>(on_did_close)
    .request::<Probe, _, _>(probe)
    .resource_policy(policy)
    .build()
    .expect("one hook per notebook built-in is a valid registration set")
}

/// Serve `server` over an in-memory transport, feeding `messages` one at a time
/// and awaiting each request's response before sending the next, so ordering is
/// observable. Closing the peer end then drains the connection.
async fn drive(server: Server<AppState>, messages: Vec<RawMessage>) -> Vec<RawMessage> {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<RawMessage>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RawMessage>();
    let transport = ChannelTransport { in_rx, out_tx };

    let handle = tokio::spawn(async move { server.serve(transport).await });

    let mut outbox = Vec::new();
    for msg in messages {
        let response_id = msg.id().cloned();
        in_tx.send(msg).unwrap();
        if let Some(response_id) = response_id {
            let response = tokio::time::timeout(Duration::from_secs(2), out_rx.recv())
                .await
                .expect("response arrived within 2s")
                .expect("writer remained open");
            assert_eq!(response.id(), Some(&response_id));
            outbox.push(response);
        }
    }
    drop(in_tx); // peer disconnect → serve drains and returns

    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("serve returned within 2s")
        .expect("server task did not panic")
        .expect("serve ended cleanly");

    outbox.extend(std::iter::from_fn(|| out_rx.try_recv().ok()));
    outbox
}

/// The `capabilities` object from the initialize response `id`.
fn probed_capabilities(outbox: &[RawMessage], id: i32) -> Value {
    let response = outbox
        .iter()
        .find(
            |m| matches!(m, RawMessage::Response { id: rid, .. } if *rid == RequestId::Number(id)),
        )
        .expect("initialize was answered");
    match response {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => {
            let body: Value = serde_json::from_slice(bytes).expect("the result decodes");
            body["capabilities"].clone()
        }
        other => panic!("expected a success response, got {other:?}"),
    }
}

fn probed(outbox: &[RawMessage], id: i32) -> ProbeResult {
    let response = outbox
        .iter()
        .find(
            |m| matches!(m, RawMessage::Response { id: rid, .. } if *rid == RequestId::Number(id)),
        )
        .expect("the request was answered");
    match response {
        RawMessage::Response {
            result: Ok(bytes), ..
        } => serde_json::from_slice(bytes).expect("the result decodes"),
        other => panic!("expected a success response, got {other:?}"),
    }
}

fn texts(probe: &ProbeResult) -> Vec<Option<&str>> {
    probe.texts.iter().map(|t| t.as_deref()).collect()
}

fn owners(probe: &ProbeResult) -> Vec<Option<&str>> {
    probe.owners.iter().map(|o| o.as_deref()).collect()
}

// --- Tests -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_notebook_journey_keeps_the_notebook_and_document_stores_in_step() {
    let seen: Log = Arc::default();
    let outbox = drive(
        observing_server(&seen),
        vec![
            initialize_request(1),
            // Open a two-cell notebook.
            notebook_did_open(
                1,
                vec![CELL_ONE, CELL_TWO],
                vec![(CELL_ONE, "one"), (CELL_TWO, "two")],
            ),
            probe_request(2),
            // Insert a third cell at the end.
            notebook_structure_change(
                2,
                2,
                0,
                vec![CELL_THREE],
                vec![(CELL_THREE, "three")],
                Vec::new(),
            ),
            probe_request(3),
            // Edit the text of cell two.
            notebook_text_change(3, CELL_TWO, 2, 0, 3, "second"),
            probe_request(4),
            // Delete cell one.
            notebook_structure_change(4, 0, 1, Vec::new(), Vec::new(), vec![CELL_ONE]),
            probe_request(5),
            // Close the notebook. The peer names no cell text Document, so the
            // cells the notebook itself listed are the only thing that can
            // release them.
            notebook_did_close(Vec::new()),
            probe_request(6),
        ],
    )
    .await;

    let opened = probed(&outbox, 2);
    assert_eq!(
        opened.notebook,
        Some(NotebookProbe {
            notebook_type: "jupyter-notebook".to_string(),
            version: 1,
            metadata: None,
            cells: vec![CELL_ONE.to_string(), CELL_TWO.to_string()],
        }),
        "open registers the notebook verbatim"
    );
    assert_eq!(
        texts(&opened),
        [Some("one"), Some("two"), None],
        "open puts every cell text Document into the document store"
    );
    assert_eq!(owners(&opened), [Some(NOTEBOOK), Some(NOTEBOOK), None]);

    let inserted = probed(&outbox, 3);
    assert_eq!(
        inserted.notebook.as_ref().map(|n| n.cells.clone()),
        Some(vec![
            CELL_ONE.to_string(),
            CELL_TWO.to_string(),
            CELL_THREE.to_string()
        ]),
        "the splice inserts the third cell at the end"
    );
    assert_eq!(inserted.notebook.as_ref().map(|n| n.version), Some(2));
    assert_eq!(
        texts(&inserted),
        [Some("one"), Some("two"), Some("three")],
        "a structural insertion opens the cell text Document it carries"
    );

    let edited = probed(&outbox, 4);
    assert_eq!(
        texts(&edited),
        [Some("one"), Some("second"), Some("three")],
        "a cell text change flows through the incremental document path"
    );
    assert_eq!(
        edited.notebook.as_ref().map(|n| n.cells.clone()),
        Some(vec![
            CELL_ONE.to_string(),
            CELL_TWO.to_string(),
            CELL_THREE.to_string()
        ]),
        "a text-only change leaves cell membership alone"
    );

    let deleted = probed(&outbox, 5);
    assert_eq!(
        deleted.notebook.as_ref().map(|n| n.cells.clone()),
        Some(vec![CELL_TWO.to_string(), CELL_THREE.to_string()]),
        "the splice removes the first cell"
    );
    assert_eq!(
        texts(&deleted),
        [None, Some("second"), Some("three")],
        "a structural deletion closes the cell text Document it names"
    );
    assert_eq!(owners(&deleted), [None, Some(NOTEBOOK), Some(NOTEBOOK)]);

    let closed = probed(&outbox, 6);
    assert_eq!(closed.notebook, None, "close removes the notebook");
    assert_eq!(
        texts(&closed),
        [None, None, None],
        "close releases every cell text Document the notebook listed"
    );
    assert_eq!(owners(&closed), [None, None, None]);

    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            Seen::Open {
                cells: vec![CELL_ONE.to_string(), CELL_TWO.to_string()],
                version: 1,
            },
            Seen::Change {
                cells: vec![
                    CELL_ONE.to_string(),
                    CELL_TWO.to_string(),
                    CELL_THREE.to_string()
                ],
                version: 2,
            },
            Seen::Change {
                cells: vec![
                    CELL_ONE.to_string(),
                    CELL_TWO.to_string(),
                    CELL_THREE.to_string()
                ],
                version: 3,
            },
            Seen::Change {
                cells: vec![CELL_TWO.to_string(), CELL_THREE.to_string()],
                version: 4,
            },
            Seen::Close {
                still_present: false,
                cell_texts: vec![None, None, None],
            },
        ],
        "every hook runs once, in receipt order, observing post-mutation state"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_notebook_save_runs_its_hook_over_the_synchronized_notebook() {
    let seen: Log = Arc::default();
    drive(
        observing_server(&seen),
        vec![
            initialize_request(1),
            notebook_did_open(1, vec![CELL_ONE], vec![(CELL_ONE, "one")]),
            notebook_did_save(),
            notebook_did_close(vec![CELL_ONE]),
            // A save after the close still reaches its hook; it observes the
            // absence the peer's own ordering left behind.
            notebook_did_save(),
        ],
    )
    .await;

    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[1],
        Seen::Save {
            still_present: true
        },
        "a save leaves the notebook synchronized"
    );
    assert_eq!(
        seen[3],
        Seen::Save {
            still_present: false
        },
        "saving an unsynchronized notebook is not a connection failure"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_out_of_range_structural_change_is_refused_without_touching_either_store() {
    let seen: Log = Arc::default();
    let outbox = drive(
        observing_server(&seen),
        vec![
            initialize_request(1),
            notebook_did_open(
                1,
                vec![CELL_ONE, CELL_TWO],
                vec![(CELL_ONE, "one"), (CELL_TWO, "two")],
            ),
            // Start past the end of the cell array.
            notebook_structure_change(
                2,
                5,
                0,
                vec![CELL_THREE],
                vec![(CELL_THREE, "three")],
                Vec::new(),
            ),
            probe_request(2),
            // In range at the start, but deleting past the end.
            notebook_structure_change(3, 1, 9, Vec::new(), Vec::new(), vec![CELL_ONE]),
            probe_request(3),
            // The connection is still usable: an in-range change still applies.
            notebook_structure_change(
                4,
                2,
                0,
                vec![CELL_THREE],
                vec![(CELL_THREE, "three")],
                Vec::new(),
            ),
            probe_request(4),
        ],
    )
    .await;

    for id in [2, 3] {
        let rejected = probed(&outbox, id);
        assert_eq!(
            rejected.notebook.as_ref().map(|n| n.cells.clone()),
            Some(vec![CELL_ONE.to_string(), CELL_TWO.to_string()]),
            "a rejected splice leaves the cell array at its prior state"
        );
        assert_eq!(
            rejected.notebook.as_ref().map(|n| n.version),
            Some(1),
            "a rejected splice does not advance the notebook version"
        );
        assert_eq!(
            texts(&rejected),
            [Some("one"), Some("two"), None],
            "a rejected splice opens no cell text Document and closes none"
        );
    }

    let accepted = probed(&outbox, 4);
    assert_eq!(
        accepted.notebook.as_ref().map(|n| n.cells.clone()),
        Some(vec![
            CELL_ONE.to_string(),
            CELL_TWO.to_string(),
            CELL_THREE.to_string()
        ]),
        "the connection survives the protocol error and keeps synchronizing"
    );

    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            Seen::Open {
                cells: vec![CELL_ONE.to_string(), CELL_TWO.to_string()],
                version: 1,
            },
            Seen::Change {
                cells: vec![
                    CELL_ONE.to_string(),
                    CELL_TWO.to_string(),
                    CELL_THREE.to_string()
                ],
                version: 4,
            },
        ],
        "a refused structural change skips its hook"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cell_text_change_is_metered_by_the_same_document_budget_as_a_text_document() {
    let seen: Log = Arc::default();
    let server = observing_server_with_policy(
        &seen,
        ResourcePolicy {
            max_document_bytes: 10,
            ..ResourcePolicy::default()
        },
    );

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            notebook_did_open(1, vec![CELL_ONE], vec![(CELL_ONE, "one")]),
            // Growing the cell past the connection's text budget is refused on
            // the same path a `textDocument/didChange` would take.
            notebook_text_change(2, CELL_ONE, 2, 0, 3, "far too much text"),
            probe_request(2),
            notebook_text_change(3, CELL_ONE, 2, 0, 3, "four"),
            probe_request(3),
        ],
    )
    .await;

    let rejected = probed(&outbox, 2);
    assert_eq!(
        texts(&rejected),
        [Some("one"), None, None],
        "an over-budget cell edit preserves the prior cell text"
    );
    assert_eq!(
        rejected.notebook.as_ref().map(|n| n.version),
        Some(1),
        "an over-budget cell edit does not advance the notebook"
    );

    let accepted = probed(&outbox, 3);
    assert_eq!(texts(&accepted), [Some("four"), None, None]);
    assert_eq!(accepted.notebook.as_ref().map(|n| n.version), Some(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_cell_open_leaves_no_orphan_cell_text_document_behind() {
    let seen: Log = Arc::default();
    let server = observing_server_with_policy(
        &seen,
        ResourcePolicy {
            max_documents: 2,
            ..ResourcePolicy::default()
        },
    );

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            // Three cells against a two-document budget: the third is refused,
            // and the first two must not survive the refusal.
            notebook_did_open(
                1,
                vec![CELL_ONE, CELL_TWO, CELL_THREE],
                vec![(CELL_ONE, "one"), (CELL_TWO, "two"), (CELL_THREE, "three")],
            ),
            probe_request(2),
        ],
    )
    .await;

    let refused = probed(&outbox, 2);
    assert_eq!(refused.notebook, None, "the notebook is never registered");
    assert_eq!(
        texts(&refused),
        [None, None, None],
        "the cells opened before the refusal are released again"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "a refused notebook open skips its hook"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notebook_and_document_count_exhaustion_follow_the_same_overload_path() {
    let seen: Log = Arc::default();
    let failures = Arc::new(Mutex::new(Vec::<ConnectionFailure>::new()));
    let recorded = Arc::clone(&failures);
    let server = Server::builder(AppState {
        seen: Arc::clone(&seen),
    })
    .notebook_document_sync(notebook_sync_options())
    .notification::<DidOpenNotebookDocument, _, _>(on_did_open)
    .request::<Probe, _, _>(probe)
    .resource_policy(ResourcePolicy {
        max_documents: 1,
        max_notebooks: 1,
        ..ResourcePolicy::default()
    })
    .on_error(move |failure| recorded.lock().unwrap().push(failure))
    .build()
    .expect("notebook hooks and finite resource limits build");

    let text_open = |uri: &str| {
        notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "text",
                    "version": 1,
                    "text": "contents"
                }
            }),
        )
    };

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            text_open("file:///one.txt"),
            text_open("file:///two.txt"),
            notebook_did_open_at(NOTEBOOK, 1, Vec::new(), Vec::new()),
            notebook_did_open_at(
                "file:///second.ipynb",
                1,
                vec!["file:///one.txt"],
                vec![("file:///one.txt", "replacement")],
            ),
            probe_request_at(2, "file:///second.ipynb", vec!["file:///one.txt"]),
        ],
    )
    .await;

    let failures = failures.lock().unwrap();
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(
        failures
            .iter()
            .all(|failure| failure.category == ConnectionFailureCategory::Overload),
        "both count budgets use the connection overload path"
    );
    assert_eq!(
        failures
            .iter()
            .map(|failure| failure.context.method.as_deref())
            .collect::<Vec<_>>(),
        [
            Some("textDocument/didOpen"),
            Some("notebookDocument/didOpen")
        ]
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the refused second notebook skips its hook"
    );
    let refused = probed(&outbox, 2);
    assert_eq!(
        refused.notebook, None,
        "the second notebook is not retained"
    );
    assert_eq!(
        texts(&refused),
        [Some("contents")],
        "the rejected notebook restores an existing cell Document"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notebook_and_document_byte_exhaustion_follow_the_same_overload_path() {
    let failures = Arc::new(Mutex::new(Vec::<ConnectionFailure>::new()));
    let recorded = Arc::clone(&failures);
    let server = Server::builder(AppState {
        seen: Arc::default(),
    })
    .notebook_document_sync(notebook_sync_options())
    .request::<Probe, _, _>(probe)
    .resource_policy(ResourcePolicy {
        max_document_bytes: 4,
        ..ResourcePolicy::default()
    })
    .on_error(move |failure| recorded.lock().unwrap().push(failure))
    .build()
    .expect("a finite document byte budget builds");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            notification(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": "file:///too-large.txt",
                        "languageId": "text",
                        "version": 1,
                        "text": "12345"
                    }
                }),
            ),
            notebook_did_open_at(
                "file:///too-large.ipynb",
                1,
                vec!["file:///too-large.ipynb#cell"],
                vec![("file:///too-large.ipynb#cell", "12345")],
            ),
            probe_request_at(
                2,
                "file:///too-large.ipynb",
                vec!["file:///too-large.ipynb#cell"],
            ),
        ],
    )
    .await;

    let failures = failures.lock().unwrap();
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(
        failures
            .iter()
            .all(|failure| failure.category == ConnectionFailureCategory::Overload),
        "both byte-budget rejections use the connection overload path"
    );
    assert_eq!(
        failures
            .iter()
            .map(|failure| failure.context.method.as_deref())
            .collect::<Vec<_>>(),
        [
            Some("textDocument/didOpen"),
            Some("notebookDocument/didOpen")
        ]
    );
    let refused = probed(&outbox, 2);
    assert_eq!(refused.notebook, None, "the notebook is not retained");
    assert_eq!(
        texts(&refused),
        [None],
        "the over-budget cell Document is not retained"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_that_never_advertised_notebook_sync_ignores_notebook_notifications() {
    // Notebook sync is opt-in: a server that never called
    // `notebook_document_sync` advertises no `notebookDocumentSync`, so a
    // conformant client sends nothing here. A peer that sends anyway must not
    // reach the notebook layer, the cell Documents, or the hook — an
    // unadvertised capability is not a back door into connection state.
    let seen: Log = Arc::default();
    let server = Server::builder(AppState {
        seen: Arc::clone(&seen),
    })
    .notification::<DidOpenNotebookDocument, _, _>(on_did_open)
    .notification::<DidCloseNotebookDocument, _, _>(on_did_close)
    .request::<Probe, _, _>(probe)
    .build()
    .expect("notebook hooks without the sync capability are a valid registration set");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            notebook_did_open(1, vec![CELL_ONE], vec![(CELL_ONE, "one")]),
            probe_request(2),
        ],
    )
    .await;

    let initialized = probed_capabilities(&outbox, 1);
    assert_eq!(
        initialized.get("notebookDocumentSync"),
        None,
        "the server advertised no notebook sync capability"
    );

    let probe = probed(&outbox, 2);
    assert_eq!(probe.notebook, None, "the notebook layer stayed empty");
    assert_eq!(
        texts(&probe),
        [None, None, None],
        "no cell text Document was opened"
    );
    assert_eq!(
        *seen.lock().expect("the log is not poisoned"),
        Vec::new(),
        "the ignored notification ran no post-mutation hook"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_built_in_mutation_runs_without_any_registered_hook() {
    let server = Server::builder(AppState {
        seen: Arc::default(),
    })
    .notebook_document_sync(notebook_sync_options())
    .request::<Probe, _, _>(probe)
    .build()
    .expect("a lone custom request builds");

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            notebook_did_open(1, vec![CELL_ONE], vec![(CELL_ONE, "one")]),
            probe_request(2),
            notebook_did_close(Vec::new()),
            probe_request(3),
        ],
    )
    .await;

    assert_eq!(texts(&probed(&outbox, 2)), [Some("one"), None, None]);
    let closed = probed(&outbox, 3);
    assert_eq!(closed.notebook, None);
    assert_eq!(texts(&closed), [None, None, None]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notebook_cell_mutations_never_reach_the_text_document_hooks() {
    // ADR 0034: a notebook notification is the single observer for the cell
    // Documents it adds, edits, and removes. It synthesizes no
    // `textDocument/*` notification, so those hooks stay silent.
    let seen: Log = Arc::default();
    let server = Server::builder(AppState {
        seen: Arc::clone(&seen),
    })
    .notebook_document_sync(notebook_sync_options())
    .notification::<DidOpenNotebookDocument, _, _>(on_did_open)
    .notification::<DidOpenTextDocument, _, _>(|state: Arc<AppState>, _ctx, _params| async move {
        state
            .seen
            .lock()
            .unwrap()
            .push(Seen::TextDocument("didOpen"));
    })
    .notification::<DidChangeTextDocument, _, _>(|state: Arc<AppState>, _ctx, _params| async move {
        state
            .seen
            .lock()
            .unwrap()
            .push(Seen::TextDocument("didChange"));
    })
    .notification::<DidCloseTextDocument, _, _>(|state: Arc<AppState>, _ctx, _params| async move {
        state
            .seen
            .lock()
            .unwrap()
            .push(Seen::TextDocument("didClose"));
    })
    .build()
    .expect("notebook and text-document hooks coexist");

    drive(
        server,
        vec![
            initialize_request(1),
            notebook_did_open(1, vec![CELL_ONE], vec![(CELL_ONE, "one")]),
            notebook_structure_change(2, 1, 0, vec![CELL_TWO], vec![(CELL_TWO, "two")], Vec::new()),
            notebook_text_change(3, CELL_ONE, 2, 0, 3, "edited"),
            notebook_structure_change(4, 0, 1, Vec::new(), Vec::new(), vec![CELL_ONE]),
            notebook_did_close(Vec::new()),
        ],
    )
    .await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![Seen::Open {
            cells: vec![CELL_ONE.to_string()],
            version: 1,
        }],
        "only the notebook hooks the server registered ran"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spliced_out_cell_is_released_even_when_the_peer_omits_it_from_did_close() {
    // A peer that removes a cell from the array but forgets to list it in
    // `didClose` would otherwise strand its Document: the cell is gone from the
    // notebook, so no later notification — `didClose` included — can name it.
    let seen: Log = Arc::default();
    let outbox = drive(
        observing_server(&seen),
        vec![
            initialize_request(1),
            notebook_did_open(
                1,
                vec![CELL_ONE, CELL_TWO],
                vec![(CELL_ONE, "one"), (CELL_TWO, "two")],
            ),
            notebook_structure_change(2, 0, 1, Vec::new(), Vec::new(), Vec::new()),
            probe_request(2),
        ],
    )
    .await;

    let spliced = probed(&outbox, 2);
    assert_eq!(
        spliced.notebook.as_ref().map(|n| n.cells.clone()),
        Some(vec![CELL_TWO.to_string()]),
        "the splice removed the first cell"
    );
    assert_eq!(
        texts(&spliced),
        [None, Some("two"), None],
        "the departed cell's text Document is released with it"
    );
    assert_eq!(owners(&spliced), [None, Some(NOTEBOOK), None]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_change_restores_every_cell_it_had_already_edited() {
    // One notification edits two cells and the second edit is over budget. The
    // first cell must not keep the text of a notification the notebook layer
    // never committed.
    let seen: Log = Arc::default();
    let server = observing_server_with_policy(
        &seen,
        ResourcePolicy {
            max_document_bytes: 20,
            ..ResourcePolicy::default()
        },
    );

    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            notebook_did_open(
                1,
                vec![CELL_ONE, CELL_TWO],
                vec![(CELL_ONE, "one"), (CELL_TWO, "two")],
            ),
            notification(
                "notebookDocument/didChange",
                json!({
                    "notebookDocument": { "uri": NOTEBOOK, "version": 2 },
                    "change": {
                        "cells": {
                            "textContent": [
                                {
                                    "document": { "uri": CELL_ONE, "version": 2 },
                                    "changes": [{ "text": "first" }]
                                },
                                {
                                    "document": { "uri": CELL_TWO, "version": 2 },
                                    "changes": [{ "text": "far beyond the budget" }]
                                }
                            ]
                        }
                    }
                }),
            ),
            probe_request(2),
        ],
    )
    .await;

    let refused = probed(&outbox, 2);
    assert_eq!(
        texts(&refused),
        [Some("one"), Some("two"), None],
        "the accepted first edit is rolled back with the refused second one"
    );
    assert_eq!(
        refused.notebook.as_ref().map(|n| n.version),
        Some(1),
        "the notebook never advanced"
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![Seen::Open {
            cells: vec![CELL_ONE.to_string(), CELL_TWO.to_string()],
            version: 1,
        }],
        "the refused change skips its hook"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_change_restores_repeated_edits_to_the_same_cell() {
    let seen: Log = Arc::default();
    let server = observing_server_with_policy(
        &seen,
        ResourcePolicy {
            max_document_bytes: 20,
            ..ResourcePolicy::default()
        },
    );
    let outbox = drive(
        server,
        vec![
            initialize_request(1),
            notebook_did_open(
                1,
                vec![CELL_ONE, CELL_TWO],
                vec![(CELL_ONE, "one"), (CELL_TWO, "two")],
            ),
            notification(
                "notebookDocument/didChange",
                json!({
                    "notebookDocument": { "uri": NOTEBOOK, "version": 2 },
                    "change": { "cells": { "textContent": [
                        {
                            "document": { "uri": CELL_ONE, "version": 2 },
                            "changes": [{ "text": "first" }]
                        },
                        {
                            "document": { "uri": CELL_ONE, "version": 3 },
                            "changes": [{ "text": "second" }]
                        },
                        {
                            "document": { "uri": CELL_TWO, "version": 2 },
                            "changes": [{ "text": "far beyond the budget" }]
                        }
                    ] } }
                }),
            ),
            probe_request(2),
        ],
    )
    .await;

    let refused = probed(&outbox, 2);
    assert_eq!(texts(&refused), [Some("one"), Some("two"), None]);
    assert_eq!(refused.notebook.as_ref().map(|n| n.version), Some(1));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![Seen::Open {
            cells: vec![CELL_ONE.to_string(), CELL_TWO.to_string()],
            version: 1,
        }],
        "a rejected batch never reaches its hook"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_out_of_range_splice_reports_an_inbound_protocol_failure() {
    let failures = Arc::new(Mutex::new(Vec::<ConnectionFailure>::new()));
    let recorded = Arc::clone(&failures);
    let server = Server::builder(AppState {
        seen: Arc::default(),
    })
    .notebook_document_sync(notebook_sync_options())
    .request::<Probe, _, _>(probe)
    .on_error(move |failure| recorded.lock().unwrap().push(failure))
    .build()
    .expect("an error hook alongside a custom request builds");

    drive(
        server,
        vec![
            initialize_request(1),
            notebook_did_open(1, vec![CELL_ONE], vec![(CELL_ONE, "one")]),
            notebook_structure_change(2, 9, 0, Vec::new(), Vec::new(), Vec::new()),
        ],
    )
    .await;

    let failures = failures.lock().unwrap();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(
        failures[0].category,
        ConnectionFailureCategory::Protocol,
        "an out-of-range splice is a protocol error, not a panic or an overload"
    );
    assert_eq!(
        failures[0].context.direction,
        Some(ConnectionDirection::Inbound)
    );
    assert_eq!(
        failures[0].context.method.as_deref(),
        Some("notebookDocument/didChange")
    );
}
