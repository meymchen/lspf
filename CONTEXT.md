# lspf

A Rust framework for building extensible LSP language servers. The framework
is async-only, and the goal is that a developer can stand up a working
language server in very little code.

## Language

**Handler**:
An async function registered to respond to an LSP method or notification.
Handlers are always `async fn` — the framework runs on an async executor on
every supported target.
_Avoid_: Feature (pygls's term), callback, endpoint.

**Built-in handler**:
A handler that the framework ships out of the box. Scope: LSP lifecycle
(`initialize`, `initialized`, `shutdown`, `exit`), text-document sync
(`didOpen`, `didChange`, `didClose`), cancellation (`$/cancelRequest`),
`$/setTrace`, workspace-folder sync, and work-done progress cancellation.
The protocol built-ins fixed by ADR 0018 cannot be replaced; registering one
of their notification methods records a post-mutation hook instead (see
[[User handler]]).
_Avoid_: Default handler — we standardize on "built-in" because it matches
how the project describes "what the framework provides by default".

**User handler**:
A handler that the user registers. For any LSP method that also has a
built-in feature handler, the user handler takes priority — override happens
via registration, not subclassing. Protocol built-ins fixed by ADR 0018 are
the exception: their validation and mutation cannot be replaced, and a
notification registration for one of those methods adds a post-mutation user
hook.
_Avoid_: Custom handler (less precise), override (the mechanism, not the
thing being registered).

**Server**:
The object (`Server<S>`) that owns exactly one LSP connection and its
initialization lifecycle, built by `Server::builder(state)` after static
registrations and served through a [[Transport]] constructor such as
`lspf::stdio(server)`. A second connection requires a second `Server`;
connection state is never shared between servers. Defined by ADR 0017.
_Avoid_: Session, backend, dispatcher (the pre-0.2 concept).

**Router**:
The permanently frozen table (`Router<S>`) of user request, notification,
and [[Command]] handlers for one connection, plus the capability catalog
those registrations imply (ADR 0017). Registrations commit through the
static builder and the single `configure_initialize` transaction; the
protocol engine then freezes the Router forever and computes
`ServerCapabilities` from it. No API mutates a frozen Router.
_Avoid_: Dispatch table, route table (descriptive, but not the type name).

**Document**:
A text resource the framework tracks on behalf of the user, kept in sync
with the editor through `textDocument/didOpen`, `didChange`, and
`didClose`. Identified by URI; carries language ID, version, and
contents.
_Avoid_: File (a document may have no on-disk file), buffer (editor-side
term).

**Documents**:
The framework-owned, concurrency-safe store of all tracked [[Document]]s
for a connection — users never construct it, store it in their state
struct, or hand it back through a getter. Mutations happen only through
the protocol engine's built-in document-sync handlers; user code reads it
through a read-only `DocumentsView` from the [[Context]] parameter
(`ctx.documents()`), and post-mutation hooks observe the updated state.
_Avoid_: Document store (correct but verbose).

**Workspace**:
The cloneable handle to workspace-folder and configuration state, exposed
through [[Context]] (ADR 0017). The protocol engine establishes it from
`InitializeParams` during initialization and owns its mutation
(`workspace/didChangeWorkspaceFolders`); user hooks observe post-mutation
state. Document contents live in [[Documents]], not here.
_Avoid_: Project, root (the LSP `rootUri` is only an input to it).

**Client**:
The cloneable typed handle for server-to-client requests and notifications,
exposed through [[Context]] (`ctx.client()`). A typed notification is
encoded and enqueued without allocating an ID; a typed request reserves a
connection-local ID and awaits its correlated response (ADR 0018).
`Client` is only a handle — the outbound queue, ID allocator, and pending
registry are owned by the connection's protocol engine.
_Avoid_: Connection (that is the transport level), sender.

**Command**:
A user-registered async closure dispatched on `workspace/executeCommand`
by name. Commands are how the user exposes custom actions to the editor
(refactorings, code generators, etc.) without inventing a new LSP method.
Distinct from a [[Handler]] in that one [[Handler]] (the built-in for
`workspace/executeCommand`) routes to many commands by string key.
_Avoid_: Action (LSP uses "code action" for something different),
custom request (a separate extension mechanism, see below).

**Context**:
The cheap-to-clone framework-state handle passed by value to every
[[Handler]] (ADR 0017, revising ADR 0009's borrowed `&Context`). Through
it handlers reach the read-only [[Documents]] view, the [[Workspace]]
handle, and the [[Client]] handle for outgoing requests and
notifications, plus the current request's scope (id, tracing span).
It is the only way a handler reaches framework state — the user's own
struct holds only user-owned state, and user code never constructs a
`Context`.
_Avoid_: Session, server-state.

**Transport**:
The message-framed channel over which LSP JSON-RPC envelopes flow into
and out of the framework. v1 ships four: stdio, TCP, WebSocket (all
native + tokio), and worker-channel (WASM in browser, wrapping a JS
`MessagePort`). The trait sees one envelope at a time; framing
(`Content-Length` for stdio/TCP, none for the others) is the adapter's
concern.
_Avoid_: Connection (overloaded with TCP-specific meaning), socket
(byte-stream connotation), channel.

**Runtime**:
The internal, crate-private trait through which the framework spawns and
cancels tasks (ADR 0020). Exactly two implementations exist —
`TokioRuntime` on native targets and `WasmRuntime` on browser WASM —
selected by compile target, with no runtime-selection API. `Runtime`
executes spawn, abort, and join but owns no protocol state; it is not
implementable or nameable by users.
_Avoid_: Executor, reactor (those are what `Runtime` delegates to).

**Layer**:
A framework-defined wrapper around a [[Service]] that adds cross-cutting
behavior to user dispatch (rate limits, audit logging, …). User Layers
are registered with `.layer(...)` and wrap only the user Service — the
last registered is outermost — while panic isolation, tracing, and
concurrency limiting are fixed framework-owned Layers outside them
(ADR 0019). Layers see decoded `IncomingCall` / `ServiceResult` values,
never transport bytes, and cannot intercept lifecycle, cancellation, or
document mutation, which are protocol built-ins. This is lspf's own
trait — narrower than `tower::Layer` and intentionally not interoperable
with it.
_Avoid_: Middleware (less precise; "layer" is the trait name and the
canonical term), interceptor.

**Service**:
The internal abstraction (`Service<State>`) that consumes one normalized
`IncomingCall` and asynchronously returns exactly one `ServiceResult`
(ADR 0019). Every [[Layer]] wraps a `Service`; the terminal
`RouterService` adapts the frozen [[Router]] and invokes the matching
typed [[Handler]]. Users never implement `Service` for their own logic —
the framework adapts their registered handlers into the terminal service.
_Avoid_: Dispatcher, endpoint.

**Default stack**:
The fixed `Service` stack installed by `lspf::stdio()`, `tcp()`,
`websocket()`, and `worker_channel()`. In v1, its outer-to-inner order is the
framework-owned panic-isolation, tracing, and bounded-concurrency [[Layer]]s
(64 in-flight by default), zero or more registered user Layers (last
registered outermost), and the terminal Router service. Registering a user
Layer does not replace any framework position. Panic isolation cannot be
disabled, and there is no all-off switch. Lifecycle, `$/cancelRequest`,
document mutation, and workspace-folder mutation are always-on
`ProtocolEngine` built-ins outside the Layer stack.
_Avoid_: Default middleware (we use "layer"), built-in middleware.

**Custom request / notification**:
A non-standard LSP method the user adds (e.g. `myExt/blame`). Registered
through the builder surface, not the trait. Distinct from a [[Command]]
because it has its own method name on the wire and isn't routed through
`workspace/executeCommand`.
_Avoid_: Extension method (overloaded with LSP's own extension
proposals).

**Test coverage**:
The proportion of source code lines or branches exercised by the test
suite. Measured by a coverage tool and reported as a percentage.
Distinct from [[helper coverage]] (the framework's built-in LSP helper
surface, see ADR 0008).
_Avoid_: Coverage (unqualified; use "test coverage" or "helper coverage").
