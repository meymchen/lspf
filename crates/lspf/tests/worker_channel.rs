#![cfg(all(feature = "worker-channel", target_arch = "wasm32"))]

use futures_channel::mpsc::{UnboundedReceiver, unbounded};
use futures_util::StreamExt;
use js_sys::Uint8Array;
use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;
use web_sys::{Event, MessageChannel, MessageEvent, MessagePort};

use lspf::{
    Error, RawMessage, Transport, TransportError, TransportReader, TransportWriter,
    WorkerChannelTransport,
};

mod conformance_support {
    pub(crate) use lspf::{LspError, Outcome, Result, Server, ServerContext, TaskSend};
}

#[path = "../src/transport/conformance.rs"]
mod conformance;

#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
import { Worker, threadId } from 'node:worker_threads';
import { fileURLToPath } from 'node:url';

export function currentThreadId() {
    return threadId;
}

export function serveConformanceInNodeWorker(serverPort) {
    const bindingsPath = fileURLToPath(
        new URL('../../wasm-bindgen-test.js', import.meta.url),
    );
    const source = `
        const { parentPort, workerData, threadId } = require('node:worker_threads');
        const wasm = require(workerData.bindingsPath);
        parentPort.once('message', async ({ serverPort }) => {
            try {
                const report = await wasm.serveConformanceInWorker(serverPort);
                parentPort.postMessage({ report, threadId });
            } catch (error) {
                parentPort.postMessage({ error: error?.stack ?? String(error) });
            } finally {
                parentPort.close();
            }
        });
    `;
    const worker = new Worker(source, {
        eval: true,
        workerData: { bindingsPath },
    });
    worker.unref();
    return new Promise((resolve, reject) => {
        worker.once('error', reject);
        worker.once('message', ({ report, threadId, error }) => {
            if (error) {
                reject(new Error(error));
                return;
            }
            resolve(JSON.stringify({ ...JSON.parse(report), workerThreadId: threadId }));
        });
        worker.postMessage({ serverPort }, [serverPort]);
    });
}

export function instrumentCloseListeners(port) {
    const closeListeners = new Set();
    const add = port.addEventListener;
    const remove = port.removeEventListener;
    Object.defineProperty(port, 'addEventListener', {
        configurable: true,
        value(type, listener, options) {
            if (type === 'close') closeListeners.add(listener);
            return add.call(this, type, listener, options);
        },
    });
    Object.defineProperty(port, 'removeEventListener', {
        configurable: true,
        value(type, listener, options) {
            if (type === 'close') closeListeners.delete(listener);
            return remove.call(this, type, listener, options);
        },
    });
    port.__lspfCloseListeners = closeListeners;
}

export function closeListenerCount(port) {
    return port.__lspfCloseListeners.size;
}
"#)]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(js_name = currentThreadId)]
    fn current_thread_id() -> u32;

    #[wasm_bindgen::prelude::wasm_bindgen(js_name = serveConformanceInNodeWorker)]
    fn serve_conformance_in_node_worker(server_port: MessagePort) -> js_sys::Promise;

    #[wasm_bindgen::prelude::wasm_bindgen(js_name = instrumentCloseListeners)]
    fn instrument_close_listeners(port: &MessagePort);

    #[wasm_bindgen::prelude::wasm_bindgen(js_name = closeListenerCount)]
    fn close_listener_count(port: &MessagePort) -> u32;
}

#[wasm_bindgen::prelude::wasm_bindgen(js_name = serveConformanceInWorker)]
pub async fn serve_conformance_in_worker(port: MessagePort) -> String {
    instrument_close_listeners(&port);
    let listener_probe = port.clone();
    let (server, task_probe) = conformance::server_with_task_probe();
    let outcome = lspf::worker_channel(server, port)
        .serve()
        .await
        .expect("the conformance Worker serves without a transport error");
    let exit_code = match outcome {
        lspf::Outcome::Exit { code } => code,
        other => panic!("the conformance Worker exits through the protocol: {other:?}"),
    };

    json!({
        "exitCode": exit_code,
        "cancelledTaskDropped": task_probe.cancelled_task_dropped(),
        "sessionCloseTaskDropped": task_probe.session_close_task_dropped(),
        "listenersRemoved": listener_probe.onmessage().is_none()
            && listener_probe.onmessageerror().is_none()
            && close_listener_count(&listener_probe) == 0,
    })
    .to_string()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerServingReport {
    exit_code: i32,
    cancelled_task_dropped: bool,
    session_close_task_dropped: bool,
    listeners_removed: bool,
    worker_thread_id: u32,
}

async fn conformance_serving_in_node_worker(
    port: MessagePort,
    exit_code: i32,
    cancelled_task_dropped: bool,
    session_close_task_dropped: bool,
) -> lspf::Result<lspf::Outcome> {
    let report = JsFuture::from(serve_conformance_in_node_worker(port))
        .await
        .expect("the conformance Worker serves successfully")
        .as_string()
        .expect("the Worker returns a JSON report");
    let report: WorkerServingReport =
        serde_json::from_str(&report).expect("the Worker report is well-formed");

    assert_ne!(report.worker_thread_id, current_thread_id());
    assert_eq!(report.exit_code, exit_code);
    assert_eq!(report.cancelled_task_dropped, cancelled_task_dropped);
    assert_eq!(
        report.session_close_task_dropped,
        session_close_task_dropped
    );
    assert!(report.listeners_removed);
    Ok(lspf::Outcome::Exit {
        code: report.exit_code,
    })
}

struct MessagePortClient {
    port: MessagePort,
    incoming: UnboundedReceiver<Value>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
}

impl MessagePortClient {
    fn new(port: MessagePort) -> Self {
        let (incoming_tx, incoming) = unbounded();
        let on_message = Closure::new(move |event: MessageEvent| {
            let value = serde_json::from_str(
                &event
                    .data()
                    .as_string()
                    .expect("the server sends JSON strings"),
            )
            .expect("the server message contains JSON");
            incoming_tx
                .unbounded_send(value)
                .expect("the test client remains open");
        });
        port.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        port.start();
        Self {
            port,
            incoming,
            _on_message: on_message,
        }
    }

    fn send(&self, message: Value) {
        self.port
            .post_message(&message.to_string().into())
            .expect("send test MessagePort message");
    }

    async fn receive(&mut self) -> Value {
        self.incoming
            .next()
            .await
            .expect("the server writes a message")
    }
}

impl conformance::WireClient for MessagePortClient {
    async fn send(&mut self, message: Value) {
        MessagePortClient::send(self, message);
    }

    async fn receive(&mut self) -> Value {
        MessagePortClient::receive(self).await
    }
}

fn assert_every_server_listener_removed(port: &MessagePort) {
    assert!(port.onmessage().is_none());
    assert!(port.onmessageerror().is_none());
    assert_eq!(close_listener_count(port), 0);
}

#[wasm_bindgen_test]
async fn worker_channel_passes_the_shared_transport_conformance_journey() {
    let channel = MessageChannel::new().expect("create a MessageChannel");
    let mut client = MessagePortClient::new(channel.port2());
    let serving = conformance_serving_in_node_worker(channel.port1(), 0, true, false);

    conformance::run(&mut client, serving).await;
}

#[wasm_bindgen_test]
async fn exit_aborts_and_joins_a_pending_handler_before_serving_resolves() {
    let channel = MessageChannel::new().expect("create a MessageChannel");
    let mut client = MessagePortClient::new(channel.port2());
    let serving = conformance_serving_in_node_worker(channel.port1(), 1, false, true);
    let closing = async {
        conformance::initialize(&mut client).await;
        client.send(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "conformance/waitForSessionClose",
            "params": {},
        }));
        let started = client.receive().await;
        assert_eq!(
            started["params"]["message"],
            "session-close handler started"
        );
        client.send(json!({ "jsonrpc": "2.0", "method": "exit" }));
    };

    let ((), outcome) = futures_util::join!(closing, serving);

    assert_eq!(
        outcome.expect("serve until exit"),
        lspf::Outcome::Exit { code: 1 }
    );
}

#[wasm_bindgen_test]
async fn strings_and_uint8_arrays_are_single_utf8_json_envelopes_in_event_order() {
    let channel = MessageChannel::new().expect("create a MessageChannel");
    let (mut reader, _writer) = WorkerChannelTransport::new(channel.port1())
        .expect("wrap the server port")
        .split();
    let peer = channel.port2();
    peer.start();
    peer.post_message(
        &json!({
            "jsonrpc": "2.0",
            "method": "worker/string",
            "params": { "text": "héllo" },
        })
        .to_string()
        .into(),
    )
    .expect("send a string envelope");
    let bytes = json!({
        "jsonrpc": "2.0",
        "method": "worker/bytes",
        "params": { "sequence": 2 },
    })
    .to_string()
    .into_bytes();
    peer.post_message(&Uint8Array::from(bytes.as_slice()).into())
        .expect("send a Uint8Array envelope");

    assert!(matches!(
        reader.recv().await.expect("receive the string envelope"),
        RawMessage::Notification { method, .. } if method == "worker/string"
    ));
    assert!(matches!(
        reader.recv().await.expect("receive the Uint8Array envelope"),
        RawMessage::Notification { method, .. } if method == "worker/bytes"
    ));
}

#[wasm_bindgen_test]
async fn framing_non_utf8_and_multiple_envelopes_are_rejected_as_malformed() {
    let cases = [
        wasm_bindgen::JsValue::from_str("Content-Length: 2\r\n\r\n{}"),
        Uint8Array::from(&b"\xff"[..]).into(),
        wasm_bindgen::JsValue::from_str(
            r#"{"jsonrpc":"2.0","method":"one"}{"jsonrpc":"2.0","method":"two"}"#,
        ),
    ];

    for data in cases {
        let channel = MessageChannel::new().expect("create a MessageChannel");
        let (mut reader, _writer) = WorkerChannelTransport::new(channel.port1())
            .expect("wrap the server port")
            .split();
        let peer = channel.port2();
        peer.start();
        peer.post_message(&data).expect("send malformed data");

        assert!(matches!(
            reader.recv().await,
            Err(TransportError::Malformed(_))
        ));
    }
}

#[wasm_bindgen_test]
async fn the_fixed_sixteen_mib_limit_applies_on_receive_and_send() {
    let channel = MessageChannel::new().expect("create a MessageChannel");
    let (mut reader, mut writer) = WorkerChannelTransport::new(channel.port1())
        .expect("wrap the server port")
        .split();
    let peer = channel.port2();
    peer.start();
    peer.post_message(&Uint8Array::new_with_length(16 * 1024 * 1024 + 1).into())
        .expect("send oversized data");
    assert!(matches!(
        reader.recv().await,
        Err(TransportError::OversizedMessage {
            length: 16_777_217,
            limit: 16_777_216,
        })
    ));

    let oversized = RawMessage::Notification {
        method: "worker/oversized".into(),
        params: bytes::Bytes::from(vec![b'x'; 16 * 1024 * 1024]),
    };
    assert!(matches!(
        writer.send(oversized).await,
        Err(TransportError::OversizedMessage {
            limit: 16_777_216,
            ..
        })
    ));
}

#[wasm_bindgen_test]
async fn peer_port_close_reaches_the_reader_and_closes_the_writer() {
    let channel = MessageChannel::new().expect("create a MessageChannel");
    let server_port = channel.port1();
    let peer = channel.port2();
    instrument_close_listeners(&server_port);
    let (mut reader, mut writer) = WorkerChannelTransport::new(server_port.clone())
        .expect("wrap the server port")
        .split();
    assert!(server_port.onmessage().is_some());
    assert!(server_port.onmessageerror().is_some());
    assert_eq!(close_listener_count(&server_port), 1);

    peer.close();

    assert!(matches!(reader.recv().await, Err(TransportError::Closed)));
    let message = RawMessage::Notification {
        method: "worker/closed".into(),
        params: bytes::Bytes::new(),
    };
    assert!(matches!(
        writer.send(message).await,
        Err(TransportError::Closed)
    ));
    drop(reader);
    assert_every_server_listener_removed(&server_port);
}

#[wasm_bindgen_test]
async fn messageerror_enters_the_protocol_engine_transport_error_path() {
    let channel = MessageChannel::new().expect("create a MessageChannel");
    let server_port = channel.port1();
    let transport = WorkerChannelTransport::new(server_port.clone()).expect("wrap the server port");
    let server = lspf::Server::builder(()).build().expect("build a server");
    let trigger = async move {
        server_port
            .dispatch_event(&Event::new("messageerror").expect("create messageerror event"))
            .expect("dispatch messageerror");
    };

    let ((), outcome) = futures_util::join!(trigger, server.serve(transport));
    assert!(matches!(
        outcome,
        Err(Error::Transport(TransportError::Malformed(detail)))
            if detail.contains("could not deserialize")
    ));
}

#[wasm_bindgen_test]
async fn peer_port_close_uses_the_common_closed_outcome_through_the_public_entry_point() {
    let channel = MessageChannel::new().expect("create a MessageChannel");
    let server_port = channel.port1();
    let peer = channel.port2();
    instrument_close_listeners(&server_port);
    let server = lspf::Server::builder(()).build().expect("build a server");
    let serving = lspf::worker_channel(server, server_port.clone()).serve();
    let closing = async move {
        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(
            &wasm_bindgen::JsValue::NULL,
        ))
        .await
        .expect("yield to serving");
        peer.close();
    };

    let ((), outcome) = futures_util::join!(closing, serving);
    assert_eq!(
        outcome.expect("peer close is a common transport ending"),
        lspf::Outcome::TransportClosed
    );
    assert_every_server_listener_removed(&server_port);
}

#[wasm_bindgen_test]
fn dropping_a_polled_public_serving_future_removes_every_listener() {
    use futures_util::FutureExt;

    let channel = MessageChannel::new().expect("create a MessageChannel");
    let server_port = channel.port1();
    instrument_close_listeners(&server_port);
    let server = lspf::Server::builder(()).build().expect("build a server");
    let serving = lspf::worker_channel(server, server_port.clone()).serve();

    assert!(serving.now_or_never().is_none(), "serving waits for input");
    assert_every_server_listener_removed(&server_port);
}
