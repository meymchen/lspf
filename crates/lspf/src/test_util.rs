//! Shared helpers for framework unit tests.
//!
//! Compiled only under `#[cfg(test)]`; nothing here is public API.

use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};

use lsp_types::Uri;

static TRACING_CAPTURE_LOCK: Mutex<()> = Mutex::new(());
static TRACING_INTEREST: OnceLock<tracing::Dispatch> = OnceLock::new();

/// Serialize scoped subscribers because `tracing` rebuilds one process-wide
/// callsite interest cache whenever a default subscriber changes.
pub(crate) fn tracing_capture_lock() -> std::sync::MutexGuard<'static, ()> {
    // Keep one dispatch registered for the test binary's lifetime so a
    // concurrent first use of a callsite cannot cache `Interest::never` while
    // another thread is inside a scoped capture. The dispatch is never made
    // current, so it retains no events; each test's scoped subscriber still
    // receives only that test thread's events.
    TRACING_INTEREST.get_or_init(|| tracing::Dispatch::new(tracing_subscriber::registry()));
    TRACING_CAPTURE_LOCK.lock().unwrap()
}

/// An absolute `file:` URI for `path`, percent-encoding every byte that is
/// not legal in a URI path.
pub(crate) fn file_uri(path: &Path) -> Uri {
    let absolute = std::path::absolute(path).expect("the test path is absolute");
    let text = absolute.to_str().expect("test paths are valid UTF-8");
    #[cfg(windows)]
    let text = text.replace('\\', "/");
    // On Windows the drive letter is part of the path, so the URI needs the
    // empty authority spelled out as a third slash: `file:///C:/…`.
    #[cfg(windows)]
    let spelling = format!("file:///{text}");
    #[cfg(not(windows))]
    let spelling = format!("file://{text}");
    Uri::from_str(&percent_encode(&spelling)).expect("the encoded test URI parses")
}

fn percent_encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Captures `tracing` events with their rendered fields, so unit tests can
/// assert what the framework emits locally. Modeled on the `EventCapture`
/// layer in `tests/outgoing_notifications.rs`.
#[derive(Clone, Default)]
pub(crate) struct EventCapture {
    events: Arc<Mutex<Vec<(tracing::Level, String)>>>,
}

impl EventCapture {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether any captured event's rendered fields contain `needle`.
    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|(_, fields)| fields.contains(needle))
    }

    /// Whether any captured event at exactly `level` has rendered fields
    /// containing `needle`.
    pub(crate) fn contains_at(&self, level: tracing::Level, needle: &str) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|(event_level, fields)| *event_level == level && fields.contains(needle))
    }

    /// How many captured events at exactly `level` have rendered fields
    /// containing `needle`.
    pub(crate) fn count_at(&self, level: tracing::Level, needle: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(event_level, fields)| *event_level == level && fields.contains(needle))
            .count()
    }

    /// The rendered field strings of every captured event.
    pub(crate) fn messages(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|(_, fields)| fields.clone())
            .collect()
    }
}

struct FieldVisitor(String);

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        self.0.push_str(&format!("{}={value:?}", field.name()));
    }
}

impl<S> tracing_subscriber::Layer<S> for EventCapture
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor(String::new());
        event.record(&mut visitor);
        self.events
            .lock()
            .unwrap()
            .push((*event.metadata().level(), visitor.0));
    }
}
