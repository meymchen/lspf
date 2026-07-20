# Typed Router and capability catalog

Status: Accepted. Supersedes
[ADR 0004](0004-capability-auto-derivation.md).
Revises the public handler-interface choices in ADRs
[0003](0003-shared-self-concurrent-dispatch.md),
[0006](0006-handler-result-and-framework-owned-error.md), and
[0009](0009-context-as-handler-parameter.md), and clarifies the user-Layer
placement in [ADR 0010](0010-own-layer-trait-not-tower.md).

Status note:
[ADR 0019](0019-protocol-invariants-and-service-layers.md) supersedes this
ADR's first-registered-outermost user-Layer composition rule with
last-registered-outermost. The historical body below is unchanged.

## Context

ADR 0004 coupled user handlers and advertised capabilities through
associated constants on a `LanguageServer` trait. That model permits the
handler table and `ServerCapabilities` to disagree, cannot express several
standard methods that contribute to one capability, and has no bounded phase
for registrations that depend on `InitializeParams`.

lspf 0.2 needs one registration model for standard LSP features, custom
requests and notifications, commands, initialization-dependent features, and
Layers. It must preserve native Rust handler signatures, derive capabilities
from the same registrations used for dispatch, and freeze all local dispatch
state before the server reports its capabilities.

This decision uses the glossary terms [[Handler]], [[User handler]],
[[Command]], [[Context]], [[Layer]], and [[Service]]. A `Client` is the
outgoing-client concern available through `Context`, not a replacement name
for `Context`. A `Workspace` is the workspace-folder and configuration
concern; it does not replace the [[Documents]] store.

## Decision

The 0.2 public registration model consists of these top-level objects:

- `ServerBuilder<S>` owns user state and collects static registrations.
- `Server<S>` owns exactly one LSP connection and its initialization
  lifecycle. A second connection requires a second `Server`.
- `Router<S>` is the permanently frozen table of user request,
  notification, and command handlers for that connection.
- `FeatureSpec` is the sealed descriptor contract for standard LSP features.
- `Context` is the cheap-to-clone framework-state handle passed to every user
  handler.
- `Client` is the cloneable outgoing request and notification handle exposed
  through `Context`.
- `Workspace` is the cloneable workspace-folder and configuration handle
  exposed through `Context`.

`ProtocolEngine`, `Service`, `Layer`, and `Runtime` are not additional
registration surfaces. `ProtocolEngine` owns JSON-RPC and LSP lifecycle
behavior; `Service` and `Layer` form the internal cross-cutting-behavior seam;
and `Runtime` owns task spawning, cancellation, and target-specific execution.
This ADR fixes only those ownership boundaries; their exact interfaces belong
to later ADRs. A Layer registered through the builder wraps only the user
`Service`; it does not wrap transport framing, JSON-RPC validation, lifecycle
handling, or capability construction. The protocol-owned default stack from
ADR 0010 may separately use internal Layers for lifecycle,
`$/cancelRequest`, panic catching, concurrency, and tracing around that user
Service.

Registration has two phases. `ServerBuilder<S>` first records and validates
static registrations. After the `ProtocolEngine` accepts exactly one valid
`initialize` request, it runs exactly one synchronous
`configure_initialize` phase against a transactional registrar. The phase
uses a no-op callback when the user does not supply one. A successful callback
commits its registrations, permanently freezes `Router<S>`, and only then
allows the engine to compute
`InitializeResult.capabilities`. No API mutates the local Router after that
point.

## Interface and behavior

### Builder surface

The normative builder flow is:

```rust
let server = lspf::Server::builder(State::new())
    .feature(lspf::features::hover(), hover_handler)
    .feature(
        lspf::features::completion(completion_options),
        completion_handler,
    )
    .request::<MyCustomRequest>(custom_request_handler)
    .notification::<MyCustomNotification>(custom_notification_handler)
    .command::<MyArgs, MyResult>("my.command", command_handler)
    .configure_initialize(|params, registrar| Ok(()))
    .layer(MyLayer)
    .build()?;
```

The names above are the public names. A purely mechanical Rust disambiguation
may add syntax but may not change these semantics:

- `Server::builder(state)` wraps `state` in one `Arc<S>` shared by every user
  handler for the connection.
- `feature(spec, handler)` registers the standard method fixed by `spec` and
  contributes the descriptor's capability data.
- `request::<R>(handler)` registers the method and wire types fixed by a
  user-implementable `lspf::types::request::Request` marker, which is lspf's
  re-export of `lsp_types::request::Request`. It contributes no server
  capability.
- `notification::<N>(handler)` registers the method and parameter type fixed
  by a user-implementable `lspf::types::notification::Notification` marker,
  which is lspf's re-export of
  `lsp_types::notification::Notification`. It contributes no server
  capability.
- `command::<Args, Output>(name, handler)` registers a typed [[Command]]
  beneath the built-in `workspace/executeCommand` user-service entry and adds
  `name` to the execute-command capability.
- `configure_initialize(callback)` replaces the no-op callback with the sole
  user-supplied initialization-dependent registration callback. Supplying it
  more than once is a `BuildError`.
- `layer(layer)` composes a [[Layer]] around the user `Service`. Calls compose
  in declaration order: the first declared Layer is outermost and observes a
  call first.
- `build()` validates the complete static registration set and returns
  `Result<Server<S>, BuildError>`. It performs no I/O and does not run
  `configure_initialize`.

The builder takes ownership of each returned builder value. Handler and Layer
registrations must satisfy the bounds required to serve concurrent calls:
`Send + Sync + 'static` on native targets. Target-specific relaxation for
WASM is owned by ADR 0020; the call shapes in this ADR do not change.

### Native handler shapes

Typed request handlers have this semantic signature:

```rust
Fn(
    Arc<S>,
    Context,
    Params,
    CancellationToken,
) -> Future<Output = Result<ResultType, LspError>>
```

Typed notification handlers have this semantic signature:

```rust
Fn(Arc<S>, Context, Params) -> Future<Output = ()>
```

Typed command handlers have this semantic signature:

```rust
Fn(
    Arc<S>,
    Context,
    Args,
    CancellationToken,
) -> Future<Output = Result<Output, LspError>>
```

On native targets, every returned future is `Send + 'static`. Request and
command cancellation uses the request-scoped `CancellationToken`;
notifications have no response and therefore no token. `Context` is passed by
value and is cheap to clone. Parameters and command arguments are deserialized
and passed by value. The framework always supplies user state as `Arc<S>`;
users do not opt into shared wrapping themselves.

Handlers reach [[Documents]], `Workspace`, and `Client` through `Context`.
They never receive or access the raw outgoing channel. `Context` may expose
convenience helpers that delegate to `Client`, but it remains the one
framework-state parameter named by the glossary.

### Type erasure and the Service seam

The Router stores `ErasedRequestHandler<S>`,
`ErasedNotificationHandler<S>`, and `ErasedCommandHandler<S>`. Registration
constructs the appropriate erased handler around the typed user handler.
Those erased handlers have exactly three responsibilities:

1. deserialize the incoming parameter or argument value once;
2. call the typed handler with native Rust values; and
3. serialize its success value or map and serialize its `LspError` once.

Malformed parameters become the protocol's invalid-params error without
calling the typed handler. A notification with malformed parameters is logged
and dropped because notifications have no response.

The internal user `Service` call carries the original incoming parameter
payload and dispatch metadata toward the erased terminal handler, and carries
the terminal handler's encoded result away from it. Layers may inspect or
replace metadata, reject a call, catch failures, or govern execution, but they
do not convert a typed handler value back through JSON. There is exactly one
parameter decode per dispatched method and exactly one result encode per
request or command; notifications have no result to encode. There is no
serialization round-trip between Layers.

### FeatureSpec and the capability catalog

`FeatureSpec` is a public sealed trait. lspf implements it for descriptor
types returned from `lspf::features::*`; downstream crates can use those
descriptors but cannot implement `FeatureSpec` and present a pseudo-standard
feature. Each descriptor fixes all of:

- one `lspf::types::request::Request` or
  `lspf::types::notification::Notification` marker, re-exported from the
  corresponding `lsp_types` trait;
- the descriptor's public options type; and
- one deterministic contribution to the internal `CapabilityBuilder`.

For example, `features::hover()` fixes the hover request marker and its hover
capability contribution, while `features::completion(options)` fixes the
completion request marker and contributes the supplied completion options.
Custom methods do not implement or pass through `FeatureSpec`; they use the
user-implementable `lspf::types::request::Request` and
`lspf::types::notification::Notification` marker traits and make no capability
contribution.

The capability catalog contains one descriptor for every supported stable
standard method. `CapabilityBuilder` merges descriptor contributions by
their destination field in `ServerCapabilities`, rather than assigning the
field once per method. In particular, it has family-specific merge logic for:

- completion and completion-item resolve;
- rename and prepare-rename;
- code lens and code-lens resolve;
- document link and document-link resolve;
- semantic tokens full, range, and full-delta support;
- document and workspace diagnostics; and
- all registered command names in execute-command support.

Merging is deterministic and independent of registration order. A companion
method such as completion-item resolve augments the same capability emitted by
completion. Semantic-token variants merge only when their legend and related
options agree. Commands merge into one de-duplicated command list.

The following are `BuildError`s during static validation or conditional
transaction validation:

- two handlers for the same request or notification method;
- two handlers for the same command name;
- a command registration that conflicts with an explicit
  `workspace/executeCommand` user handler;
- two contributions that provide different values for an option that must be
  singular within one capability family;
- a dependent method whose required base method or base options are absent;
  and
- any incompatible semantic-token legend, mode, or delta contribution.

An identical duplicate is still an error because it leaves handler precedence
ambiguous. Capability construction never uses last-write-wins behavior.
`BuildError` identifies the conflicting method or command and the capability
field involved.

### Initialization transaction and Router freeze

`configure_initialize` receives read-only `InitializeParams` and
`&mut InitializeRegistrar<S>`. The parameters cannot be mutated or retained
by reference. `InitializeRegistrar<S>` offers the same `feature`, `request`,
`notification`, and `command` registration semantics as the static builder,
but no `layer`, nested `configure_initialize`, `build`, or dynamic-client
operation.

The callback is synchronous:

```rust
FnOnce(
    &InitializeParams,
    &mut InitializeRegistrar<S>,
) -> Result<(), LspError>
```

It cannot return a future or `.await`. The engine invokes it only after
protocol validation of the first `initialize` request and never invokes it for
an invalid or repeated `initialize`. The registrar begins with a transactional
view of all static registrations.

On `Ok(())`, the framework validates the combined registration set. Success
atomically commits the conditional registrations and freezes `Router<S>`
permanently. The engine then builds capabilities from the frozen catalog plus
protocol-owned negotiated fields such as ADR 0016's position encoding, stores
the finalized connection state, and returns `InitializeResult`.

If the callback returns `Err`, or if combined validation finds a conflict, the
framework discards every conditional registration. The `initialize` request
receives JSON-RPC `InternalError`; no partial Router or partial capability set
becomes observable, and that `Server` does not proceed to the initialized
state.

Outgoing dynamic-registration helpers on `Client` may send
`client/registerCapability` and `client/unregisterCapability` after
initialization. They describe client-side routing expectations only. They
never add, replace, or remove a local Router entry and never recompute
`InitializeResult.capabilities`.

### Protocol stability boundary

The default capability catalog targets stable LSP 3.17. lspf pins
`lsp-types` to the `0.97.x` release line, consistent with ADR 0014; updating
outside that line requires an explicit compatibility decision.

Any proposed method or 3.18-draft method, descriptor, capability field, marker
re-export, or helper is available only behind lspf's `proposed` Cargo feature.
Enabling `proposed` extends the catalog without changing the registration and
freeze rules in this ADR. Stable applications do not see or advertise those
items by default.

`workspace/textDocumentContent/refresh` is not a default stable helper.
Notebook methods and notebook protocol types are excluded from the v1 Router
catalog, matching ADR 0008. A custom marker can still name an application
extension method, but doing so does not make it a catalog feature or advertise
a standard capability.

## Rejected alternatives

We rejected the `LanguageServer` trait plus associated capability constants
from ADR 0004. It permits capabilities and dispatch to drift, spreads a
multi-method capability across unrelated constants, and cannot safely commit
initialization-dependent registrations.

We rejected open implementation of `FeatureSpec`. Allowing downstream
pseudo-standard descriptors would make lspf responsible for merging arbitrary
capability fragments and would erase the stable-versus-proposed boundary.
Typed custom `Request` and `Notification` markers provide the extension seam
without claiming catalog support.

We rejected raw string methods as the primary API. Marker types give each
method one parameter and result contract at compile time. We also rejected
giving every method a capability fragment: custom methods have no standard
capability to advertise.

We rejected asynchronous or repeated initialization hooks. Awaiting user work
while the Router is mutable complicates cancellation and lets registration
escape its transaction. Repeated hooks make capability output and dispatch
depend on timing.

We rejected mutating the local Router from dynamic-registration helpers.
Client registration and server dispatch have different ownership; coupling
them would make an outgoing request able to silently replace local behavior.

We rejected serializing values at every Layer boundary. Layers govern a
Service call, not a chain of JSON transports, so repeated conversion adds cost
and can change values without adding a useful isolation boundary.

## Consequences

One source of truth now determines dispatch and advertised capabilities.
Static conflicts fail before I/O, conditional conflicts fail the initialize
transaction, and handler precedence never depends on registration order.
Standard feature discovery is more explicit than a trait impl, but examples
can show each handler and its advertised options together.

The Router is connection-owned and immutable during normal message handling,
which simplifies concurrent dispatch. Servers that accept multiple clients
must construct one `Server<S>` per connection and choose explicitly whether
their application state is shared above those servers.

The sealed catalog makes adding a stable or proposed standard feature an lspf
release task: the implementation must add its descriptor, erased adaptation,
capability merge rule, and tests. In return, downstream users cannot create
capability combinations the framework does not know how to validate.

Synchronous conditional registration cannot perform network or filesystem
I/O. Applications must load such data before building the Server or make the
conditional choice solely from already-available state and
`InitializeParams`.

## Migration impact

ADR 0004 is superseded. The `LanguageServer` capability constants and
`server_capabilities()` override are not part of the 0.2 registration model.
Existing implementations migrate each standard trait method to
`.feature(descriptor, handler)`, carrying its former constant value into the
descriptor options.

Custom methods migrate to `.request::<R>` or `.notification::<N>`. Existing
execute-command dispatch maps migrate each named branch to `.command`. Logic
that formerly overrode `server_capabilities()` moves either into standard
descriptor options or, when it depends on the client, into the single
`configure_initialize` transaction.

User state that previously lived on a `LanguageServer` receiver becomes the
value passed to `Server::builder` and arrives as `Arc<S>`. Framework state and
outgoing helpers move behind the by-value `Context`; handlers do not retain a
raw sender.

This call shape deliberately revises ADR 0003's `&self` receiver and ADR
0009's `&Context` parameter. It preserves their concurrency and explicit
dependency decisions: the framework still shares state through an `Arc`,
dispatches user work concurrently, and passes `Context` explicitly, but the
typed free handler receives owned clones of both handles. ADR 0006's
framework-owned `LspError` and unit-returning notifications remain unchanged;
its `Option<T>` is now carried by the request marker's exact `ResultType`
where the protocol method is optional.

ADR 0010's default stack also remains. Builder-registered Layers are placed at
the user `Service` seam inside that protocol-owned stack, so they cannot
intercept transport, lifecycle validation, or capability construction.

ADR 0016's position-encoding negotiation remains protocol-owned. It runs after
the Router freezes and augments the capability result without becoming a user
`FeatureSpec`.

## Tests required by downstream milestones

The builder and Router milestones must add public-interface tests proving:

- each standard descriptor dispatches typed parameters and derives the
  matching capability;
- custom request and notification markers dispatch without changing
  capabilities;
- commands dispatch typed arguments and results and merge into one
  execute-command capability;
- every listed multi-method capability family merges compatible
  contributions independent of registration order;
- duplicates, missing base methods, and conflicting capability contributions
  return a diagnostic `BuildError`, never last-write-wins;
- request and command cancellation tokens reach the typed handler, while
  notification handlers have the unit-returning shape;
- malformed parameters never call the typed handler;
- instrumented values cross exactly one decode and one encode with zero
  Layer-induced serialization round-trips;
- the initialize callback runs synchronously and exactly once only after a
  valid `initialize`;
- a failed conditional callback or combined validation discards its entire
  transaction and returns `InternalError`;
- capabilities are computed from the permanently frozen Router after a
  successful transaction;
- dynamic-registration helpers do not mutate or recompute the local Router;
- default builds expose only stable 3.17 catalog entries, while `proposed`
  builds expose draft entries without exposing notebook support; and
- one `Server` rejects a second connection lifecycle while two separately
  built servers operate independently.
