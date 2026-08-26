//! End-to-end coverage for editing and document-presentation descriptors.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use lspf::types::{
    Color, ColorInformation, ColorPresentation, ColorPresentationParams, ColorProviderOptions,
    DocumentColorParams, DocumentFormattingOptions, DocumentFormattingParams, Position, Range,
    TextEdit,
};
use lspf::{
    BuildError, CancellationToken, LspError, RawMessage, RequestId, Server, ServerContext,
    Transport, TransportError, TransportReader, TransportWriter,
};

struct AppState;

async fn format_document(
    _: Arc<AppState>,
    _: ServerContext,
    _: DocumentFormattingParams,
    _: CancellationToken,
) -> Result<Option<Vec<TextEdit>>, LspError> {
    Ok(Some(vec![TextEdit {
        range: Range::new(Position::new(0, 0), Position::new(0, 3)),
        new_text: "formatted".to_string(),
    }]))
}

async fn document_colors(
    _: Arc<AppState>,
    _: ServerContext,
    _: DocumentColorParams,
    _: CancellationToken,
) -> Result<Vec<ColorInformation>, LspError> {
    Ok(vec![ColorInformation {
        range: Range::new(Position::new(1, 0), Position::new(1, 7)),
        color: Color {
            red: 1.0,
            green: 0.0,
            blue: 0.0,
            alpha: 1.0,
        },
    }])
}

async fn color_presentations(
    _: Arc<AppState>,
    _: ServerContext,
    _: ColorPresentationParams,
    _: CancellationToken,
) -> Result<Vec<ColorPresentation>, LspError> {
    Ok(vec![ColorPresentation {
        label: "#ff0000".to_string(),
        text_edit: None,
        additional_text_edits: None,
    }])
}

fn formatting_options() -> DocumentFormattingOptions {
    DocumentFormattingOptions {
        work_done_progress_options: lspf::types::WorkDoneProgressOptions {
            work_done_progress: Some(true),
        },
    }
}

fn server() -> Server<AppState> {
    Server::builder(AppState)
        .feature(
            lspf::features::document_formatting(formatting_options()),
            format_document,
        )
        .feature(
            lspf::features::document_color(ColorProviderOptions {}),
            document_colors,
        )
        .feature(lspf::features::color_presentation(), color_presentations)
        .build()
        .expect("editing and presentation features build")
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
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        self.out_tx
            .send(message)
            .map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

fn request(id: i32, method: &'static str, params: Value) -> RawMessage {
    RawMessage::Request {
        id: RequestId::Number(id),
        method: Cow::Borrowed(method),
        params: Bytes::from(serde_json::to_vec(&params).unwrap()),
    }
}

fn initialize_request() -> RawMessage {
    request(
        1,
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

async fn drive(messages: Vec<RawMessage>) -> Vec<RawMessage> {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let transport = ChannelTransport { in_rx, out_tx };
    let mut handle = tokio::spawn(async move { server().serve(transport).await });
    let mut server_done = false;
    let mut outbox = Vec::new();

    'messages: for message in messages {
        let response_id = message.id().cloned();
        if in_tx.send(message).is_err() {
            break;
        }
        if let Some(response_id) = response_id {
            tokio::select! {
                response = out_rx.recv() => {
                    let Some(response) = response else {
                        server_done = true;
                        break 'messages;
                    };
                    assert_eq!(response.id(), Some(&response_id));
                    outbox.push(response);
                }
                result = &mut handle => {
                    result.expect("server task did not panic").expect("serve ended cleanly");
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
            .expect("serve returned")
            .expect("server task did not panic")
            .expect("serve ended cleanly");
    }
    outbox.extend(std::iter::from_fn(|| out_rx.try_recv().ok()));
    outbox
}

fn result(outbox: &[RawMessage], id: i32) -> Value {
    let response = outbox
        .iter()
        .find(|message| message.id() == Some(&RequestId::Number(id)))
        .expect("response id");
    let RawMessage::Response {
        result: Ok(bytes), ..
    } = response
    else {
        panic!("successful response")
    };
    serde_json::from_slice(bytes).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn representative_editing_and_presentation_handlers_dispatch_through_the_engine() {
    let outbox = drive(vec![
        initialize_request(),
        request(
            2,
            "textDocument/formatting",
            json!({
                "textDocument": { "uri": "file:///a.rs" },
                "options": { "tabSize": 4, "insertSpaces": true }
            }),
        ),
        request(
            3,
            "textDocument/documentColor",
            json!({
                "textDocument": { "uri": "file:///a.rs" }
            }),
        ),
        request(
            4,
            "textDocument/colorPresentation",
            json!({
                "textDocument": { "uri": "file:///a.rs" },
                "color": { "red": 1.0, "green": 0.0, "blue": 0.0, "alpha": 1.0 },
                "range": {
                    "start": { "line": 1, "character": 0 },
                    "end": { "line": 1, "character": 7 }
                }
            }),
        ),
        exit(),
    ])
    .await;

    let capabilities = &result(&outbox, 1)["capabilities"];
    assert_eq!(
        capabilities["documentFormattingProvider"]["workDoneProgress"],
        true
    );
    assert_eq!(capabilities["colorProvider"], json!({}));
    assert_eq!(result(&outbox, 2)[0]["newText"], "formatted");
    assert_eq!(result(&outbox, 3)[0]["color"]["red"], 1.0);
    assert_eq!(result(&outbox, 4)[0]["label"], "#ff0000");
}

#[test]
fn duplicate_editing_route_and_orphan_color_presentation_fail_deterministically() {
    let duplicate = Server::builder(AppState)
        .feature(
            lspf::features::document_formatting(formatting_options()),
            format_document,
        )
        .feature(
            lspf::features::document_formatting(formatting_options()),
            format_document,
        )
        .build()
        .err()
        .expect("duplicate formatting route fails");
    assert_eq!(
        duplicate,
        BuildError::DuplicateMethod("textDocument/formatting".to_string())
    );

    let orphan = Server::builder(AppState)
        .feature(lspf::features::color_presentation(), color_presentations)
        .build()
        .err()
        .expect("color presentation needs document color");
    assert_eq!(
        orphan,
        BuildError::ConflictingCapability {
            field: "colorProvider"
        }
    );
}
