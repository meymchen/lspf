//! The log format this server installs.
//!
//! `LSPF_LOG_FORMAT=json` writes one machine-readable event per line, which is
//! what an editor's output channel is given by default; anything else writes
//! the compact text below. A crate cannot share a module with another crate's
//! examples, so this file and `crates/lspf/examples/example_logging/mod.rs`
//! are kept identical by hand.

use std::fmt;
use std::time::Instant;

use tracing::{Event, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

/// Install the process-wide subscriber.
///
/// Logs go to stderr: stdout carries the LSP wire protocol and nothing else.
pub(crate) fn init() {
    let filter = EnvFilter::from_default_env();
    if std::env::var("LSPF_LOG_FORMAT").as_deref() == Ok("json") {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(filter)
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(false)
            .init();
        return;
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        // Editor output channels display ANSI bytes as visible symbols.
        .with_ansi(false)
        .event_format(Text::new())
        .init();
}

/// One line per event, sized for an editor output channel.
///
/// The framework records `connection_id`, `direction`, `kind`, `method`, and
/// `request_id` on the event itself and again on the enclosing span, so the
/// default format prints each of them twice. This one keeps the span names,
/// which place a handler's own event in the call that produced it, and drops
/// the span fields the event already carries. It drops the target for the same
/// reason — `lspf::telemetry` on every framework line — and measures time from
/// process start, so the gap between two lines reads as a latency instead of
/// as a difference between two UTC dates.
struct Text {
    start: Instant,
}

impl Text {
    fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl<S, N> FormatEvent<S, N> for Text
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        // Seconds with a fixed width and unit: `Duration`'s own `Debug` picks
        // between µs, ms, and s per line, which unaligns the column that a
        // reader scans down.
        write!(
            writer,
            "{:>8.3}s {:<5} ",
            self.start.elapsed().as_secs_f64(),
            event.metadata().level(),
        )?;
        if let Some(scope) = ctx.event_scope() {
            let mut seen = false;
            for span in scope.from_root() {
                write!(writer, "{}:", span.metadata().name())?;
                seen = true;
            }
            if seen {
                writer.write_char(' ')?;
            }
        }
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}
