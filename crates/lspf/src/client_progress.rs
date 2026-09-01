//! Client-side work-done progress token validation and ordered delivery.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gen_lsp_types::{ProgressParams, ProgressToken};

/// One link in a per-token FIFO established synchronously during dispatch.
pub(crate) struct ProgressDelivery {
    previous: Option<futures_channel::oneshot::Receiver<()>>,
    done: Option<futures_channel::oneshot::Sender<()>>,
}

pub(crate) struct ProgressDeliveryGuard {
    _done: Option<futures_channel::oneshot::Sender<()>>,
}

impl ProgressDelivery {
    fn queued(entry: &mut ProgressEntry) -> Self {
        let previous = entry.tail.take();
        let (done, tail) = futures_channel::oneshot::channel();
        entry.tail = Some(tail);
        Self {
            previous,
            done: Some(done),
        }
    }

    fn terminal(mut entry: ProgressEntry) -> Self {
        Self {
            previous: entry.tail.take(),
            done: None,
        }
    }

    pub(crate) async fn wait(mut self) -> ProgressDeliveryGuard {
        if let Some(previous) = self.previous.take() {
            let _ = previous.await;
        }
        ProgressDeliveryGuard {
            _done: self.done.take(),
        }
    }
}

/// Whether a completed `window/workDoneProgress/create` handler established
/// its reserved token.
#[derive(Clone, Copy)]
pub(crate) enum CreateOutcome {
    Succeeded,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProgressPhase {
    Creating,
    Created,
    Begun,
}

struct ProgressEntry {
    phase: ProgressPhase,
    tail: Option<futures_channel::oneshot::Receiver<()>>,
}

/// Connection-local work-done progress tokens created by the server or
/// supplied by the Client in an outgoing request.
///
/// State transitions happen in receive order. The per-token delivery tail
/// also preserves that order while handlers await nested server calls and the
/// connection read loop continues processing their responses.
#[derive(Clone, Default)]
pub(crate) struct ClientProgressRegistry {
    inner: Arc<Mutex<HashMap<ProgressToken, ProgressEntry>>>,
}

impl ClientProgressRegistry {
    pub(crate) fn try_reserve_create(&self, token: ProgressToken) -> bool {
        self.try_insert(token, ProgressPhase::Creating)
    }

    pub(crate) fn try_register_request(&self, token: ProgressToken) -> bool {
        self.try_insert(token, ProgressPhase::Created)
    }

    fn try_insert(&self, token: ProgressToken, phase: ProgressPhase) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.contains_key(&token) {
            return false;
        }
        inner.insert(token, ProgressEntry { phase, tail: None });
        true
    }

    pub(crate) fn remove(&self, token: &ProgressToken) {
        self.inner.lock().unwrap().remove(token);
    }

    pub(crate) fn finish_create(&self, token: &ProgressToken, outcome: CreateOutcome) {
        let mut inner = self.inner.lock().unwrap();
        match (inner.get(token).map(|entry| entry.phase), outcome) {
            (Some(ProgressPhase::Creating), CreateOutcome::Succeeded) => {
                inner.get_mut(token).unwrap().phase = ProgressPhase::Created;
            }
            (Some(ProgressPhase::Creating), CreateOutcome::Failed) => {
                inner.remove(token);
            }
            _ => {}
        }
    }

    pub(crate) fn accept(&self, params: &ProgressParams) -> Option<ProgressDelivery> {
        let mut inner = self.inner.lock().unwrap();
        let phase = inner.get(&params.token).map(|entry| entry.phase);
        match (
            params.value.get("kind").and_then(serde_json::Value::as_str),
            phase,
        ) {
            (Some("begin"), Some(ProgressPhase::Created)) => {
                let entry = inner.get_mut(&params.token).unwrap();
                entry.phase = ProgressPhase::Begun;
                Some(ProgressDelivery::queued(entry))
            }
            (Some("report"), Some(ProgressPhase::Begun)) => Some(ProgressDelivery::queued(
                inner.get_mut(&params.token).unwrap(),
            )),
            (Some("end"), Some(ProgressPhase::Begun)) => Some(ProgressDelivery::terminal(
                inner.remove(&params.token).unwrap(),
            )),
            _ => None,
        }
    }
}
