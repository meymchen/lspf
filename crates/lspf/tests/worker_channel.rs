#![cfg(all(feature = "worker-channel", target_arch = "wasm32"))]

use futures_channel::mpsc::{UnboundedReceiver, unbounded};
use futures_util::StreamExt;
use js_sys::Uint8Array;
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_test::wasm_bindgen_test;
use web_sys::{Event, MessageChannel, MessageEvent, MessagePort};

use lspf::{
    Error, RawMessage, Transport, TransportError, TransportReader, TransportWriter,
    WorkerChannelTransport,
};

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

#[wasm_bindgen_test]
async fn initialize_shutdown_and_exit_cross_the_public_worker_channel_entry_point() {
    let channel = MessageChannel::new().expect("create a MessageChannel");
    let mut client = MessagePortClient::new(channel.port2());
    let server = lspf::Server::builder(()).build().expect("build a server");
    let server_port = channel.port1();
    let serving = lspf::worker_channel(server, server_port.clone()).serve();
    let journey = async {
        client.send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "processId": null, "rootUri": null, "capabilities": {} },
        }));
        assert_eq!(client.receive().await["id"], 1);

        client.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
        client.send(json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }));
        let shutdown = client.receive().await;
        assert_eq!(shutdown["id"], 2);
        assert_eq!(shutdown["result"], Value::Null);

        client.send(json!({ "jsonrpc": "2.0", "method": "exit" }));
    };

    let ((), outcome) = futures_util::join!(journey, serving);
    assert_eq!(
        outcome.expect("serve the worker channel"),
        lspf::Outcome::Exit { code: 0 }
    );
    assert!(server_port.onmessage().is_none());
    assert!(server_port.onmessageerror().is_none());
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
async fn dropping_the_reader_closes_the_writer_and_removes_both_listeners() {
    let channel = MessageChannel::new().expect("create a MessageChannel");
    let server_port = channel.port1();
    let (reader, mut writer) = WorkerChannelTransport::new(server_port.clone())
        .expect("wrap the server port")
        .split();
    assert!(server_port.onmessage().is_some());
    assert!(server_port.onmessageerror().is_some());

    drop(reader);

    assert!(server_port.onmessage().is_none());
    assert!(server_port.onmessageerror().is_none());
    let message = RawMessage::Notification {
        method: "worker/closed".into(),
        params: bytes::Bytes::new(),
    };
    assert!(matches!(
        writer.send(message).await,
        Err(TransportError::Closed)
    ));
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
    assert!(server_port.onmessage().is_none());
    assert!(server_port.onmessageerror().is_none());
}

#[wasm_bindgen_test]
fn dropping_a_polled_public_serving_future_removes_every_listener() {
    use futures_util::FutureExt;

    let channel = MessageChannel::new().expect("create a MessageChannel");
    let server_port = channel.port1();
    let server = lspf::Server::builder(()).build().expect("build a server");
    let serving = lspf::worker_channel(server, server_port.clone()).serve();

    assert!(serving.now_or_never().is_none(), "serving waits for input");
    assert!(server_port.onmessage().is_none());
    assert!(server_port.onmessageerror().is_none());
}
