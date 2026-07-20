# ProtocolEngine and the outbound request broker

Status: Accepted. Extends the ownership boundaries and uses the type names and
registration model fixed by
[ADR 0017](0017-typed-router-and-capability-catalog.md). Clarifies ownership
of the unbounded outbound queue selected by
[ADR 0015](0015-outgoing-channel-unbounded-send-loop.md).

Status note:
[ADR 0019](0019-protocol-invariants-and-service-layers.md) supersedes this
ADR's references to `.no_default_layers()`; there is no all-off switch and
panic isolation is always installed. The historical body below is unchanged.

## Context

ADR 0017 makes `Router<S>` immutable after initialization and identifies
`ProtocolEngine` as the owner of JSON-RPC and LSP lifecycle behavior. It does
not place the mutable connection state that coordinates the reader, Writer,
user handlers, and `Client`. Leaving that state split among the Router,
Layers, and Transport adapters would make close races, cancellation, and
request ID correlation depend on adapter-specific choices.

The server also needs one outbound request broker. A server-to-client request
must reserve an ID before it is sent, remain pending while unrelated responses
arrive, and complete when its matching response arrives. The broker has the
same lifetime and failure boundary as the connection, so it cannot be a
process-global facility or state privately owned by each `Client` clone.

This decision fixes the connection-owned protocol module, its invariants, and
the boundary between non-replaceable protocol behavior and user handlers.
The public registration vocabulary remains that of ADR 0017:
`ServerBuilder<S>`, `Server<S>`, `Router<S>`, `Context`, `Client`,
`Workspace`, `Service`, `Layer`, and the typed handler forms.

## Decision

`ProtocolEngine` is an internal deep module. It is not a direct user-facing
interface or an additional registration surface. One `ProtocolEngine` exists
for each `Server<S>` connection and exclusively owns all mutable protocol
coordination for that connection:

- lifecycle state;
- the inbound in-flight request registry;
- the outbound pending request registry;
- the outbound request ID allocator;
- session cancellation;
- the task group;
- the outbound queue;
- the Writer failure signal;
- the `Workspace`; and
- `Router<S>` freeze state.

The work-done progress cancellation registry used by the built-in
`window/workDoneProgress/cancel` notification is protocol state and is also
owned by `ProtocolEngine`.

`Router<S>` contains only the registration table and capability catalog
defined by ADR 0017. Once the initialization transaction commits,
`ProtocolEngine` freezes the Router permanently and records that freeze in
engine state. The Router cannot freeze itself, reopen itself, or own lifecycle
state.

User Layers wrap only the user `Service`. Router, user Layers, internal
Layers, and Transport adapters do not hold or mutate lifecycle state, either
request registry, the request ID allocator, session cancellation, the task
group, the outbound queue, the Writer failure signal, the `Workspace`, or
Router freeze state. A Transport adapter frames and moves messages. A
`Runtime` executes engine-requested spawning and cancellation primitives, but
does not own the connection task group or its cancellation policy.

`Context`, `Client`, and `Workspace` are cloneable, capability-limited handles
into engine-owned facilities. Cloning a handle does not create or transfer
ownership of the underlying queue, registry, allocator, or workspace state.
Only `ProtocolEngine` creates, mutates, closes, and destroys those facilities.

The engine maintains these invariants:

1. Every valid inbound request gets exactly one response.
2. Every outbound response correlates to its request by ID, and outbound
   responses may arrive in any order.
3. A duplicate inbound ID never overwrites or cancels the existing in-flight
   task.
4. Closing the session completes every outbound pending request.
5. Writer failure and reader EOF converge on one idempotent session-close
   path.
6. Lifecycle, cancellation, and document-ordering enforcement is always on
   and cannot be removed by layer configuration.

The following protocol behavior is implemented by `ProtocolEngine` as
built-ins that users cannot replace:

1. initialize precedence and the initialization transaction;
2. shutdown and exit state transitions;
3. `$/cancelRequest`;
4. inbound correlation of responses to outbound requests;
5. `textDocument/didOpen`, `textDocument/didChange`, and
   `textDocument/didClose` mutation;
6. workspace-folder mutation;
7. `$/setTrace`; and
8. the work-done progress cancellation registry and
   `window/workDoneProgress/cancel`.

These built-ins remain installed when `.no_default_layers()` disables the
Default stack. A user registration with the method name of a built-in does not
replace or shadow the built-in. For protocol-built-in notifications other than
`exit`, ADR 0017's `.notification::<N>(handler)` registration records the one
post-mutation hook instead of a Router entry. The three lifecycle hooks use
the dedicated builder methods defined below.

## Interface and behavior

### Inbound dispatch and completion

The reader gives each decoded JSON-RPC envelope to `ProtocolEngine`, not
directly to the Router. The engine classifies responses before requests and
notifications so a response can complete a `Client` operation without
entering user dispatch.

For each valid inbound request, the engine reserves its ID in the inbound
in-flight registry before spawning user work. Normal success, `LspError`,
malformed parameters, cancellation, panic conversion, and lifecycle rejection
all select one terminal response through the same completion gate. Completing
the gate removes the registry entry and enqueues its response exactly once.

If an inbound ID is already present, the engine leaves the original task and
registry entry unchanged and responds to the duplicate with the protocol
invalid-request error. It never uses last-write-wins registration and never
redirects a later `$/cancelRequest` away from the original task.

The engine serializes lifecycle operations, built-in cancellation, and
document mutations ahead of user dispatch. Layer composition and handler
concurrency cannot bypass this ordering. Notifications still have no response,
but decode and built-in validation failures are logged consistently.

### Outbound request broker and Client

`Client` is the typed server-to-client request and notification handle exposed
through `Context`. It is not the protocol owner. A typed notification is
encoded and offered to the engine-owned outbound queue without allocating an
ID or a pending entry.

For a typed request, `ProtocolEngine` performs these steps in order:

1. reject the operation if session close has begun;
2. allocate the next connection-local outbound request ID;
3. insert a pending completion under that ID;
4. encode and enqueue the request carrying that ID; and
5. await the typed completion through `Client`.

The pending entry exists before enqueue, so a fast response cannot outrun
registration. If encoding or enqueue fails, the engine removes and completes
that entry with the same failure. Request IDs are unique among pending
requests and are not reused while pending.

When an outbound response arrives, `ProtocolEngine` removes the matching
pending entry and completes it with the decoded result or `LspError`.
Responses can arrive in any order because lookup is exclusively by ID.
An unknown, duplicate, or late response is logged and ignored; it cannot
complete another request.

### Session close and task ownership

Reader EOF, Writer failure, explicit transport closure, and fatal protocol
termination all request the same idempotent engine close operation. The first
caller records the close cause and begins closure; later callers observe the
same operation and do not repeat cleanup.

The close operation stops new outbound requests, triggers session
cancellation, closes the outbound queue, completes every outbound pending
request with a framework-owned session-closed `LspError`, cancels every
remaining task in the engine-owned task group, and then joins every task.
This cancel-then-join policy is identical for every close cause; no task is
detached. No pending `Client` future remains unresolved after close.

The Writer reports its terminal failure through the engine-owned Writer
failure signal. It does not clear registries or independently cancel tasks.
Likewise, a Transport adapter reports EOF but performs no protocol cleanup.
This convergence prevents a Writer failure racing reader EOF from completing
pending requests or cancelling tasks twice.

### Built-in ordering and notification hooks

Users may register at most one hook for each built-in notification. Duplicate
hook registration is a `BuildError`. Every such notification has fixed
ordering:

1. decode typed parameters;
2. perform built-in validation and mutation; and
3. invoke the user notification hook.

The hook observes the post-mutation `Context` state. If decoding or built-in
validation fails, the hook does not run. A hook cannot suppress, roll back, or
reorder built-in behavior. In particular, document hooks observe the updated
[[Documents]] state, workspace-folder hooks observe the updated `Workspace`,
and cancellation and trace hooks observe the updated protocol state.

The three lifecycle hooks have dedicated builder methods:

- `on_initialize` has the request-handler state, `Context`, params, and
  `CancellationToken` shape fixed by ADR 0017. It runs after the `Workspace`
  has been established and before capabilities are returned. It returns
  `Result<Option<ServerInfo>, LspError>`. It cannot register routes, mutate
  `Router<S>`, contribute capabilities, or replace the framework-generated
  `ServerCapabilities`.
- `on_shutdown` has the request-handler state, `Context`, params, and
  `CancellationToken` shape. It runs before the engine enters
  shutting-down. The engine enters shutting-down only when the hook returns
  success; on `LspError`, it remains in the initialized state and sends that
  error response.
- `on_exit` has the notification-handler state, `Context`, and params shape.
  It runs before the engine computes the exit outcome. The outcome is then
  determined from protocol-owned lifecycle state, including whether shutdown
  completed; the hook cannot choose or replace it.

`configure_initialize` from ADR 0017 remains the one synchronous,
transactional place for initialization-dependent route registration.
Initialize precedence is one total engine-owned sequence:

1. validate and reserve the connection's sole `initialize` request before any
   Router or user Layer dispatch;
2. run `configure_initialize` and validate its transactional registrations;
3. atomically commit the transaction and permanently freeze the Router;
4. establish the `Workspace` from `InitializeParams`;
5. negotiate protocol-owned fields and generate `ServerCapabilities` from
   the frozen catalog;
6. run `on_initialize`; and
7. combine its optional `ServerInfo` with the generated capabilities, enter
   initialized state, and enqueue the `InitializeResult`.

If `configure_initialize` or its combined registration validation fails, the
engine discards the transaction, sends ADR 0017's `InternalError`, records a
terminal initialization failure, and enters the single session-close path
after that response is enqueued. If `on_initialize` returns `LspError`, the
engine sends that error, records the same terminal initialization failure,
and enters the close path after the response is enqueued. Neither failure
enters initialized state or permits another `initialize` request. The close
path releases an already-established `Workspace` and an already-frozen Router
without exposing either to later dispatch.

`on_initialize` is separate from `configure_initialize` and cannot receive an
`InitializeRegistrar<S>`. Its optional `ServerInfo` is combined with the
framework-generated capabilities in the `InitializeResult`; it never replaces
them.

Initialize precedence, shutdown/exit validation, response correlation, and
the completion gates surround the user `Service` rather than passing through
user Layers. The user hooks themselves execute with their corresponding
handler shapes, but their placement does not expose the protocol registries.

## Rejected alternatives

We rejected putting lifecycle state or either request registry in
`Router<S>`. The Router is an immutable dispatch table after initialization;
connection progress and request completion are mutable protocol concerns.
Mixing them would weaken the permanent-freeze guarantee from ADR 0017.

We rejected giving each `Client` clone its own allocator or pending map.
Responses are connection-wide and may arrive out of order, so split brokers
could allocate duplicate IDs or consume one another's responses. A `Client`
is a typed handle into the single engine-owned broker.

We rejected allowing Transport adapters or the Writer task to clean up
pending requests. Adapter-specific cleanup creates two close authorities and
makes EOF and write failure race. Adapters report events; the engine performs
the one close operation.

We rejected expressing lifecycle, cancellation, response correlation, or
document synchronization as removable user Layers. Those behaviors establish
protocol validity and ordering before the user `Service`; making them
optional would invalidate the engine invariants.

We rejected replacement registrations for built-in notifications. A
post-mutation hook preserves extensibility while guaranteeing that framework
state is current before user code observes the event.

We rejected letting `on_initialize` alter capabilities or register routes.
ADR 0017 already provides the transactional `configure_initialize` registrar
for route and capability changes. Combining the two hooks would reopen the
Router or permit the returned capabilities to disagree with dispatch.

## Consequences

All connection-lifetime coordination has one owner and one close authority.
The reader, Writer, Router, Layers, Transport adapters, and user-facing
handles consequently have narrower responsibilities. Failures from either
transport direction resolve the same pending work and cannot compete to
destroy session state.

Outbound request handling requires a pending allocation before enqueue and
one map lookup per response. This small cost provides deterministic
correlation, out-of-order completion, and a guarantee that session close wakes
every caller.

Built-ins are less replaceable than ordinary standard-feature handlers.
Dedicated post-mutation hooks provide observation and extension without
allowing user code to disable validation or corrupt framework-owned state.
Applications that need different fundamental lifecycle semantics must build
outside this framework boundary.

The engine becomes a deep internal module with substantial responsibility,
but its surface is smaller than distributing registries and close logic
across multiple objects. `Client`, `Context`, and `Workspace` remain cheap
public handles without exposing protocol storage.

## Migration impact

This ADR does not change the ADR 0017 registration names or typed handler
shapes. Implementations following ADR 0017 must place connection lifecycle,
Router freeze, task ownership, workspace state, and both request directions'
registries behind `ProtocolEngine` rather than adding those fields to
`Router<S>`, Layers, or a Transport adapter.

The outbound channel behavior selected by ADR 0015 remains unbounded and
drained by a dedicated send-loop task. Its ownership is now assigned to
`ProtocolEngine`; existing prototypes with the sender, receiver, or send-loop
lifetime owned by a dispatcher or Transport must move that ownership behind
the engine.

User code that previously intended to replace a document, cancellation,
workspace, trace, or lifecycle notification must use its dedicated
post-mutation hook. Initialization-dependent feature registration remains in
`configure_initialize`; non-registration initialization work and optional
`ServerInfo` move to `on_initialize`.

`Client` implementations must route every typed request through the
connection's engine-owned ID allocator and pending registry. A direct
request-write helper that waits on the Transport or owns a private response
map is incompatible with this decision.

## Tests required by downstream milestones

The protocol-engine, Router, Client, and Transport milestones must add
public-interface or protocol-boundary tests proving:

- every valid inbound request reaches exactly one terminal response across
  success, `LspError`, malformed params, cancellation, panic, and lifecycle
  rejection;
- duplicate inbound IDs receive an invalid-request response without
  replacing or cancelling the original in-flight task;
- concurrent typed `Client` requests receive unique IDs and correlate by ID
  when responses arrive in reverse and mixed order;
- unknown, duplicate, and late outbound response IDs do not complete another
  request;
- enqueue and encoding failure remove and complete their pending outbound
  entries;
- session close completes all pending outbound requests and rejects new ones;
- simultaneous Writer failure and reader EOF execute one close path, report
  one close cause, and perform cleanup once;
- every close cause cancels and then joins the complete task group without
  detaching a task;
- Router, Layers, Writer, and Transport adapters cannot access or mutate
  engine-owned lifecycle, registry, allocator, queue, cancellation, task,
  workspace, or freeze state;
- lifecycle, cancellation, response correlation, document ordering, and
  workspace ordering remain active when `.no_default_layers()` disables the
  Default stack;
- each built-in notification hook runs once after successful decode and
  built-in mutation, and does not run after decode or validation failure;
- duplicate built-in hook registration returns `BuildError`, while a normal
  handler registration cannot replace a built-in;
- `on_initialize` sees the established `Workspace`, receives the ADR 0017
  request-handler arguments, returns optional `ServerInfo`, cannot register
  routes, and cannot replace generated `ServerCapabilities`;
- failure in `configure_initialize`, combined registration validation, or
  `on_initialize` produces its fixed error response, never enters initialized
  state, rejects another initialize, and enters the one close path;
- `configure_initialize` remains the only initialization-dependent
  registration transaction and the Router is permanently frozen before
  capabilities are returned;
- `on_shutdown` runs before transition, transitions only on success, and
  leaves the initialized state unchanged on `LspError`;
- `on_exit` runs before the exit outcome is computed and cannot override the
  lifecycle-derived outcome;
- document open, change, and close hooks observe the post-mutation
  [[Documents]] state;
- workspace-folder, `$/setTrace`, `$/cancelRequest`, and
  `window/workDoneProgress/cancel` hooks observe post-mutation engine state;
  and
- closing one `Server<S>` connection resolves only its own broker, task
  group, `Workspace`, and handles while another connection continues
  independently.
