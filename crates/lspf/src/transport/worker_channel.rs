//! One browser or Node Worker session over a JavaScript [`MessagePort`].

use std::cell::Cell;
use std::rc::Rc;

use bytes::Bytes;
use futures_channel::mpsc::{UnboundedReceiver, unbounded};
use futures_util::StreamExt;
use js_sys::Uint8Array;
use serde_json::value::RawValue;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Event, MessageEvent, MessagePort};

use super::{Transport, TransportError, TransportReader, TransportWriter, envelope};
use crate::Server;
use crate::raw::RawMessage;

const DEFAULT_MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Serve one built [`Server`] through an already-created JavaScript
/// [`MessagePort`].
///
/// The JavaScript host owns the Worker and the `MessageChannel`; this adapter
/// starts only the supplied port and never terminates the Worker.
pub fn worker_channel<S>(server: Server<S>, port: MessagePort) -> WorkerChannelBuilder<S>
where
    S: Send + Sync + 'static,
{
    WorkerChannelBuilder { server, port }
}

/// One worker-channel session ready to be served.
pub struct WorkerChannelBuilder<S> {
    server: Server<S>,
    port: MessagePort,
}

impl<S> WorkerChannelBuilder<S>
where
    S: Send + Sync + 'static,
{
    /// Start the supplied port and serve until the common protocol-engine
    /// close path completes.
    pub async fn serve(self) -> crate::Result<crate::Outcome> {
        self.server
            .serve(WorkerChannelTransport::new(self.port)?)
            .await
    }
}

/// A message-framed transport over one JavaScript [`MessagePort`].
pub struct WorkerChannelTransport {
    reader: WorkerChannelReader,
    writer: WorkerChannelWriter,
}

/// Event-backed read half of a [`WorkerChannelTransport`].
pub struct WorkerChannelReader {
    port: MessagePort,
    incoming: UnboundedReceiver<Result<RawMessage, TransportError>>,
    closed: Rc<Cell<bool>>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_message_error: Closure<dyn FnMut(Event)>,
    on_close: Closure<dyn FnMut(Event)>,
}

/// Serialized write half of a [`WorkerChannelTransport`].
///
/// The protocol engine owns exactly one value of this type in its send loop,
/// preserving outbound order without locks or background JavaScript tasks.
pub struct WorkerChannelWriter {
    port: MessagePort,
    closed: Rc<Cell<bool>>,
}

impl WorkerChannelTransport {
    /// Wrap and start `port`, using the fixed 16 MiB UTF-8 envelope limit.
    pub fn new(port: MessagePort) -> Result<Self, TransportError> {
        let (incoming_tx, incoming) = unbounded();
        let closed = Rc::new(Cell::new(false));

        let message_tx = incoming_tx.clone();
        let closed_on_message = Rc::clone(&closed);
        let on_message = Closure::new(move |event: MessageEvent| {
            if closed_on_message.get() {
                return;
            }
            let result = parse_event_data(event.data());
            if message_tx.unbounded_send(result).is_err() {
                closed_on_message.set(true);
            }
        });

        let message_error_tx = incoming_tx.clone();
        let closed_on_error = Rc::clone(&closed);
        let on_message_error = Closure::new(move |_event: Event| {
            closed_on_error.set(true);
            let _ = message_error_tx.unbounded_send(Err(TransportError::Malformed(
                "MessagePort could not deserialize an incoming message".to_string(),
            )));
        });

        // Browsers expose MessagePort as an EventTarget but currently do not
        // dispatch `close`; Node's standards-compatible MessagePort does. The
        // listener makes a host/peer close observable where the platform
        // provides it, while local shutdown below closes browser ports
        // directly. Both paths still converge through TransportError::Closed.
        let closed_on_close = Rc::clone(&closed);
        let on_close = Closure::new(move |_event: Event| {
            closed_on_close.set(true);
            let _ = incoming_tx.unbounded_send(Err(TransportError::Closed));
        });
        port.add_event_listener_with_callback("close", on_close.as_ref().unchecked_ref())
            .map_err(|error| {
                TransportError::Malformed(format!(
                    "failed to register MessagePort close listener: {error:?}"
                ))
            })?;
        // Install infallible handler properties only after the fallible
        // listener registration succeeds, so every constructor error leaves
        // the port untouched and no dropped Rust closure registered in JS.
        port.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        port.set_onmessageerror(Some(on_message_error.as_ref().unchecked_ref()));
        port.start();

        Ok(Self {
            reader: WorkerChannelReader {
                port: port.clone(),
                incoming,
                closed: Rc::clone(&closed),
                _on_message: on_message,
                _on_message_error: on_message_error,
                on_close,
            },
            writer: WorkerChannelWriter { port, closed },
        })
    }
}

impl Transport for WorkerChannelTransport {
    type Reader = WorkerChannelReader;
    type Writer = WorkerChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (self.reader, self.writer)
    }
}

impl TransportReader for WorkerChannelReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.incoming
            .next()
            .await
            .unwrap_or(Err(TransportError::Closed))
    }
}

impl Drop for WorkerChannelReader {
    fn drop(&mut self) {
        self.closed.set(true);
        self.port.set_onmessage(None);
        self.port.set_onmessageerror(None);
        let _ = self
            .port
            .remove_event_listener_with_callback("close", self.on_close.as_ref().unchecked_ref());
    }
}

impl TransportWriter for WorkerChannelWriter {
    async fn send(&mut self, msg: RawMessage) -> Result<(), TransportError> {
        if self.closed.get() {
            return Err(TransportError::Closed);
        }
        let body = envelope::serialize(&msg)?;
        enforce_limit(body.len())?;
        let text = String::from_utf8(body)
            .map_err(|error| TransportError::Malformed(error.to_string()))?;
        self.port
            .post_message(&text.into())
            .map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        self.close();
        Ok(())
    }
}

impl WorkerChannelWriter {
    fn close(&self) {
        self.closed.set(true);
        self.port.close();
    }
}

impl Drop for WorkerChannelWriter {
    fn drop(&mut self) {
        self.close();
    }
}

fn parse_event_data(data: wasm_bindgen::JsValue) -> Result<RawMessage, TransportError> {
    let body = if let Some(text) = data.as_string() {
        text.into_bytes()
    } else if let Ok(array) = data.dyn_into::<Uint8Array>() {
        array.to_vec()
    } else {
        return Err(TransportError::Malformed(
            "MessagePort data must be a string or Uint8Array".to_string(),
        ));
    };
    enforce_limit(body.len())?;
    std::str::from_utf8(&body).map_err(|error| {
        TransportError::Malformed(format!("MessagePort data is not UTF-8: {error}"))
    })?;
    serde_json::from_slice::<&RawValue>(&body)
        .map_err(|error| TransportError::Malformed(format!("invalid JSON envelope: {error}")))?;
    Ok(envelope::parse(Bytes::from(body)))
}

fn enforce_limit(length: usize) -> Result<(), TransportError> {
    if length > DEFAULT_MAX_MESSAGE_SIZE {
        Err(TransportError::OversizedMessage {
            length,
            limit: DEFAULT_MAX_MESSAGE_SIZE,
        })
    } else {
        Ok(())
    }
}
