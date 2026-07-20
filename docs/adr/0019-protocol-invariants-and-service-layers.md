# Protocol invariants and Service/Layer stack

Status: Accepted. Refines the `Service` and `Layer` decisions in
[ADR 0010](0010-own-layer-trait-not-tower.md) and
[ADR 0017](0017-typed-router-and-capability-catalog.md). Extends the
non-replaceable protocol boundary in
[ADR 0018](0018-protocol-engine-and-outbound-request-broker.md) and
supersedes its references to `.no_default_layers()`.

## Context

ADR 0010 selects lspf-owned `Service` and `Layer` traits, but its early
default-stack sketch puts protocol behavior and cross-cutting user dispatch
in the same removable stack. ADR 0017 narrows user Layers to the user
`Service` seam, fixes one parameter decode and one result encode, and leaves
the exact seam to a later decision. ADR 0018 assigns lifecycle, cancellation,
document synchronization, and workspace mutation to `ProtocolEngine`, while
retaining historical references to `.no_default_layers()`.

Layer authors need one stable call shape and one unambiguous composition
order. The framework also needs a boundary that cannot let a Layer disable
protocol validity, observe serialized bytes, or escape panic isolation.

This decision uses the type names fixed by ADRs 0017 and 0018:
`ServerBuilder<S>`, `Server<S>`, `Router<S>`, `ProtocolEngine`, `Context`,
`Service`, `Layer`, `LspError`, and the typed handler forms. `RouterService`
is the terminal `Service<State>` adapter over the frozen `Router<State>`.

## Decision

`Service<State>` handles only normalized user calls. Its call and result
shapes are:

```text
IncomingCall { kind, method, request_id, params, context }

ServiceResult::Response(raw_result)
ServiceResult::Error(LspError)
ServiceResult::NoResponse
```

The fixed stack, from outermost to innermost, is:

```text
PanicIsolation → Tracing → ConcurrencyLimit → UserLayers
(registration order, last registered outermost) → RouterService
```

All five positions are fixed. `PanicIsolation`, `Tracing`, and
`ConcurrencyLimit` are framework-owned. Each call to
`ServerBuilder<S>::layer` adds one user Layer; the last registered user Layer
is outermost among user Layers. `RouterService` is always the terminal
service.

There is no `.no_default_layers()` API. In v1, `PanicIsolation` is always
installed. A later decision may provide separate configuration for tracing
and concurrency, but it may not introduce an all-off switch or move panic
isolation inside a user Layer.

`ProtocolEngine` behavior is outside the entire Layer stack. Lifecycle,
`$/cancelRequest`, document mutation, and workspace-folder mutation are
non-replaceable built-ins. No framework or user Layer can disable, replace,
reorder, or roll them back. Layers wrap only user dispatch, including a user
hook that the engine invokes after a successful built-in mutation.

Layers receive decoded parameters and results. The parameter value and raw
result cross the Layer chain as in-memory, method-erased values, never as
transport bytes or a JSON string. The name `raw_result` means that the result
is erased from its method-specific Rust type; it does not mean serialized
wire data. There is no encode/decode boundary between Layers.

## Interface and behavior

### Normalized calls

`ProtocolEngine` validates and classifies an incoming JSON-RPC envelope before
constructing an `IncomingCall`. Each field has one meaning:

- `kind` distinguishes a request from a notification. An
  `workspace/executeCommand` call remains a request at this seam; command-name
  dispatch happens inside `RouterService`.
- `method` is the validated LSP or custom method name used by
  `RouterService`.
- `request_id` contains the validated JSON-RPC ID for a request and is absent
  for a notification.
- `params` is the decoded parameter value. It is not a byte buffer or
  serialized JSON string.
- `context` is the request- or notification-scoped `Context` backed by the
  current connection's `ProtocolEngine`.

`Service<State>` consumes one `IncomingCall` and asynchronously returns
exactly one `ServiceResult`. `RouterService` resolves `method` in the frozen
`Router<State>`, performs the ADR 0017 erased-handler adaptation, and invokes
the matching typed handler.

The result variants have fixed use:

- `Response(raw_result)` is a successful request result in decoded,
  method-erased form.
- `Error(LspError)` is a failed request result.
- `NoResponse` is the terminal result for a notification.

A Layer may inspect the normalized metadata and decoded values, replace a
decoded parameter or result, return a request error, or short-circuit a
notification with `NoResponse`. It cannot produce a response for a
notification, change a call's `kind`, or add, remove, or replace its
`request_id`. `ProtocolEngine` owns the completion gate and converts the
final `ServiceResult` into the one permitted protocol outcome.

### Composition and panic isolation

For:

```rust
Server::builder(state)
    .layer(First)
    .layer(Second)
    .build()?
```

the effective stack is:

```text
PanicIsolation → Tracing → ConcurrencyLimit → Second → First → RouterService
```

Inbound observation therefore runs through `Second` before `First`, and
results unwind through `First` before `Second`. This last-registered-outermost
rule supersedes ADR 0017's first-registered-outermost rule.

`PanicIsolation` surrounds tracing, concurrency control, every user Layer,
and `RouterService`. A panic in a user Layer or user handler therefore cannot
unwind into `ProtocolEngine` or terminate the connection task. For a request,
panic isolation returns `ServiceResult::Error` containing the framework-owned
internal-error `LspError`; for a notification, it logs the panic and returns
`ServiceResult::NoResponse`. The engine-owned completion gate still enforces
exactly one response for every valid request.

`Tracing` surrounds concurrency acquisition, so the call span includes time
spent waiting for a concurrency permit. `ConcurrencyLimit` acquires its
permit before entering any user Layer and retains the permit until the
`ServiceResult` returns.

### Protocol boundary

The reader sends decoded envelopes to `ProtocolEngine`, not to a Layer.
Before user dispatch, `ProtocolEngine` performs all applicable built-in
validation and mutation. After user dispatch, it owns response completion,
outbound encoding, and enqueueing. In particular:

- initialize, shutdown, and exit state transitions never enter the stack as
  replaceable operations;
- `$/cancelRequest` mutates the engine-owned in-flight registry before any
  post-mutation user hook runs;
- `textDocument/didOpen`, `textDocument/didChange`, and
  `textDocument/didClose` mutate [[Documents]] before any post-mutation user
  hook runs; and
- workspace-folder notifications mutate `Workspace` before any post-mutation
  user hook runs.

A user hook is a normalized user call after the built-in succeeds. A Layer
may wrap or short-circuit that hook, but doing so cannot affect the completed
built-in validation or mutation. Decode or built-in validation failure does
not enter the Layer stack.

### Single serialization boundary

The one incoming JSON decode produces the normalized parameter
representation. ADR 0017's erased handler converts that representation to the
method's native Rust parameter type without an intermediate JSON
serialization. On success, the erased handler converts the native return
value once into `raw_result`; `ProtocolEngine` later includes that value in
the one outbound JSON encoding.

Every Layer receives the same in-memory `IncomingCall` and
`ServiceResult` representations. Layer composition never serializes a value
to pass it to the next Layer and never deserializes a returned value. A Layer
that changes `params` or `raw_result` changes the decoded representation
directly.

## Rejected alternatives

We rejected an all-or-nothing default-stack switch. It makes protocol
correctness depend on a performance or observability setting and gives user
code a path to disable lifecycle, cancellation, or state synchronization.

We rejected implementing lifecycle, `$/cancelRequest`, document mutation, or
workspace-folder mutation as Layers. They must run before optional user
dispatch and must retain the engine-owned ordering and completion guarantees
from ADR 0018.

We rejected placing user Layers outside `PanicIsolation`. A panic in an
outer user Layer would bypass the framework's error conversion and could
terminate the connection task.

We rejected first-registered-outermost composition. Builder calls naturally
wrap the service built so far; last-registered-outermost gives every
registration one mechanical interpretation and fixes the examples and tests
to that interpretation.

We rejected exposing transport bytes or serialized JSON at the Service seam.
Doing so would let every Layer add a decode/encode cycle, violating ADR 0017
and making values depend on Layer count.

## Consequences

Layer authors have one narrow, decoded interface for audit logging, rate
limits, request shaping, and similar user-dispatch concerns. They cannot
intercept framing, JSON-RPC validation, capability construction, protocol
mutation, or response correlation.

Panic isolation remains a small mandatory cost for every user call.
Tracing includes concurrency wait time, and the concurrency permit covers the
complete user Layer chain. Those semantics make latency and in-flight limits
consistent regardless of registered Layers.

The separation between `ProtocolEngine` and `Service<State>` makes protocol
correctness independent of Layer configuration. A short-circuiting or
panicking user Layer can suppress user dispatch, but cannot suppress a
protocol mutation that precedes it.

Decoded-value access is more useful than byte access for Layer authors, but a
Layer that replaces parameters or results is responsible for preserving the
method contract. `RouterService` reports an invalid-params `LspError` if
replacement parameters do not deserialize to the registered handler type.

## Migration impact

ADR 0010 remains the decision to own narrow lspf-specific `Layer` and
`Service` traits instead of depending on `tower`. Its removable default-stack
sketch is superseded: implementations must remove `.no_default_layers()` and
must place protocol built-ins outside the Layer stack.

Implementations following ADR 0017 must change user Layer composition from
first-registered-outermost to last-registered-outermost. They must also use
the `IncomingCall` and `ServiceResult` shapes fixed here while preserving
ADR 0017's typed erased-handler boundary and single decode/encode rule.

Implementations following ADR 0018 must remove its historical
`.no_default_layers()` path and the corresponding downstream test premise.
Lifecycle, cancellation, document mutation, and workspace-folder mutation
remain `ProtocolEngine` built-ins; this ADR narrows the stack around the
post-built-in user call.

No v1 compatibility shim retains `.no_default_layers()`. Future independent
tracing or concurrency configuration must leave `PanicIsolation` and all
`ProtocolEngine` built-ins installed.

## Tests required by downstream milestones

The Service, Layer, Router, and ProtocolEngine milestones must add tests at
their public builder or protocol boundaries proving:

- an instrumented terminal service receives all five `IncomingCall` fields
  and each valid call returns exactly one of the three `ServiceResult`
  variants;
- `.layer(First).layer(Second)` observes inbound calls in
  `Second`, `First`, `RouterService` order and results in the reverse order;
- `PanicIsolation`, `Tracing`, and `ConcurrencyLimit` occupy the fixed
  positions outside all user Layers;
- a panic in either a user Layer or user handler becomes internal error for a
  request, becomes `NoResponse` for a notification, and leaves the connection
  able to process a later call;
- concurrency permits are acquired before user Layers and held until their
  returned result completes;
- tracing spans include concurrency-permit wait time;
- no builder surface exposes `.no_default_layers()`, and any future tracing
  or concurrency setting leaves panic isolation installed;
- lifecycle, `$/cancelRequest`, document mutation, and workspace-folder
  mutation remain active with zero user Layers and with short-circuiting or
  panicking user Layers;
- built-in notification hooks observe post-mutation [[Documents]],
  `Workspace`, or cancellation state, while decode and validation failures do
  not enter the Layer stack; and
- instrumented parameter and result values cross the complete Layer chain
  with one decode and one encode and no Layer-induced serialization
  round-trip.
