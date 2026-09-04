---
title: Set resource and observability policies
description: Bound connection resources and emit useful, payload-free telemetry.
---

One `Server` or `Client` owns one LSP connection. Its `ResourcePolicy`, queues,
deadlines, and tracing identity end with that connection. A Server's
`Documents`, `Notebooks`, and `Workspace` are connection-scoped too. A process
that serves several peers constructs one endpoint per peer and owns any shared
cache or index outside lspf.

This guide covers production budgets, observability, deployment, shutdown, and
troubleshooting. The authoritative support matrix and maintenance window are
in the repository's [`SECURITY.md`](https://github.com/meymchen/lspf/blob/main/SECURITY.md).

## Start with the finite defaults

`ResourcePolicy::default()` applies these per-connection limits:

| Field | Default | What remains charged |
| --- | ---: | --- |
| `max_inbound_requests` | 64 | Admitted requests until their single completion path wins. |
| `max_outbound_messages` | 1,024 | Accepted messages until the Transport send finishes or fails. |
| `max_outbound_bytes` | 16 MiB | Encoded JSON-RPC envelope bodies in the outbound queue. |
| `max_documents` | 1,024 | Open text documents, including notebook cells. |
| `max_document_bytes` | 64 MiB | Retained text across those documents. |
| `max_notebooks` | 256 | Notebook-level metadata, including empty notebooks. |
| `outbound_request_timeout` | 30 seconds | Each typed request sent to the peer. |
| `handler_timeout` | 30 seconds | Each admitted inbound request handler. |

All numeric limits and enabled deadlines must be greater than zero. A bad
policy is a `BuildError`, before any I/O starts. Only
`outbound_request_timeout` may be `None`; disable it only when another owner
provides a firm deadline.

Install the whole policy rather than scattering unrelated knobs:

```rust
use std::time::Duration;

use lspf::{ResourcePolicy, Server};

# struct State;
# fn main() {
let policy = ResourcePolicy {
    max_inbound_requests: 32,
    max_outbound_messages: 256,
    max_outbound_bytes: 4 * 1024 * 1024,
    max_documents: 2_000,
    max_document_bytes: 128 * 1024 * 1024,
    max_notebooks: 128,
    outbound_request_timeout: Some(Duration::from_secs(10)),
    handler_timeout: Duration::from_secs(20),
};

let server = Server::builder(State)
    .resource_policy(policy)
    .build()
    .expect("the production resource policy is valid");
# let _ = server;
# }
```

`ServerBuilder::concurrency_limit` remains as shorthand for
`max_inbound_requests`; do not set both in different configuration paths.

## Tune from retained cost and latency

Measure realistic document sizes, concurrent editor requests, response sizes,
and slow-peer behavior before raising a limit. The defaults are safety bounds,
not throughput targets.

- If inbound capacity rejects bursts, first inspect handler latency and
  cancellation. Raising the limit makes more work and memory live at once.
- If optional notifications hit `ClientError::OutboundOverloaded`, coalesce or
  drop obsolete application updates before enlarging the queue.
- If a required response cannot fit, the connection closes as
  `Outcome::WriterFailed`; the endpoint never silently drops required traffic.
- If document admission fails, the built-in leaves the previous snapshot
  untouched and skips the post-mutation hook. Notebook cells use the same
  document budgets.
- If deadlines expire during valid work, separate queue delay, external I/O,
  blocking CPU work, and a genuinely slow peer before choosing a new value.

Use the [performance baselines](https://github.com/meymchen/lspf/blob/main/docs/performance-baselines.md) for the
repository's measured request workloads and
[soak journeys](https://github.com/meymchen/lspf/blob/main/docs/soak-journeys.md) for bounded-memory stress
journeys. They are reference workloads, not production sizing promises.

## Keep blocking work bounded

All handlers are async. Move blocking libraries to `spawn_blocking`, check the
request `CancellationToken` between work units, and put an application-owned
limit around expensive jobs. The inbound request budget prevents unbounded
protocol admission; it does not limit the runtime's blocking pool, subprocesses,
database connections, or memory held in application state.

The [errors and cancellation guide](../errors-and-cancellation/) has a
cancellation-aware blocking example.

## Emit useful, payload-free telemetry

At `lspf=trace`, the framework emits stable `rpc message`,
`resource budget changed`, `deadline changed`, `request completed`, and
`connection closed` events:

| `message` | Fields |
| --- | --- |
| `rpc message` | `connection_id`, `direction`, `kind`, and, when present, `method` and `request_id` |
| `resource budget changed` | `connection_id`, `resource`, `resource_action`, `resource_current`; bounded resources also include `resource_limit`, byte budgets include `resource_bytes` and `resource_bytes_limit`, and `pending_requests` includes `direction`, `kind`, `method`, `request_id`, and optional `deadline_ms` |
| `deadline changed` | `connection_id`, `direction`, `kind`, `method`, `request_id`, `deadline`, `deadline_action`, `deadline_ms`, `deadline_elapsed_ms` |
| `request completed` | `connection_id`, `direction`, `kind`, `method`, `request_id`, `latency_ms`, `completion` |
| `connection closed` | `connection_id`, `close_cause` |

`direction` is `inbound` or `outbound`. Resource names are
`inbound_requests`, `outbound_queue`, `documents`, `notebooks`, and
`pending_requests`. Resource actions are `admit`, `release`, `update`,
`reject`, and `rollback`. Deadline names are `handler` and
`outbound_request`; deadline actions are `armed`, `completed`, `cancelled`, and
`expired`. Completion values are `success`, `error`, `cancelled`,
`deadline_expired`, `rejected`, and `connection_closed`. Close causes are
`exit`, `reader_eof`, `reader_failed`, `writer_failed`, and
`initialize_failed`. Optional fields are absent rather than set to a sentinel
value.

Request and notification spans carry the same connection and call identity.
Request spans also retain the debug-formatted `id` field for compatibility.

Route stdio server logs to stderr. Stdout is the LSP byte stream; one human
line on stdout corrupts framing. For a process supervisor, prefer structured
stderr and include your own build or instance identity outside protocol
payloads.

Register `ServerBuilder::on_error` for metrics that must not parse logs. The
hook provides a `ConnectionFailureCategory` plus redacted connection and call
identity. It deliberately omits parameters, results, document contents, wire
bytes, panic payloads, and underlying error text.

```rust
# struct State;
let _server = lspf::Server::builder(State)
    .on_error(|failure| {
        eprintln!(
            "connection {}: {:?}",
            failure.context.connection_id,
            failure.category,
        );
    })
    .build()
    .expect("server configuration is valid");
```

Numeric request IDs retain their value. Peer-controlled string IDs expose only
their `ConnectionRequestId::String` variant, with the contents redacted.
Method names are included only when they are framework-owned, registered, or
declared by a typed outbound request. Other peer-controlled method names are
omitted. A panic in the hook is caught and logged; it cannot interrupt cleanup.

The default tracing events have the same payload rule. Application events can
still leak source text, paths, request arguments, or remote messages, so review
those fields before sending logs to a shared backend.

Continue with [Deploy and troubleshoot endpoints](../deployment-and-troubleshooting/) for process topology, shutdown, limitations, and failure diagnosis.
