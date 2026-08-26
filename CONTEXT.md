# lspf

A Rust framework for building extensible LSP language servers. The framework
is async-only, and the goal is that a developer can stand up a working
language server in very little code.

## Language

**Handler**:
An async function registered to respond to an LSP method or notification.
Handlers are always `async fn` — the framework runs on an async executor on
every supported target.
_Avoid_: Feature, callback, endpoint.

**Built-in handler**:
A handler that the framework ships out of the box. Scope: LSP lifecycle
(`initialize`, `initialized`, `shutdown`, `exit`), text-document sync
(`didOpen`, `didChange`, `didClose`, `willSave`, `didSave`), cancellation
(`$/cancelRequest`), `$/setTrace`, workspace-folder sync, and work-done
progress cancellation. The protocol built-ins fixed by ADR 0018 and ADR 0023
cannot be replaced; registering one of their notification methods records a
post-validation hook instead (see [[User handler]]).
_Avoid_: Default handler — we standardize on "built-in" because it matches
how the project describes "what the framework provides by default".

**User handler**:
A handler that the user registers. For any LSP method that also has a
built-in feature handler, the user handler takes priority — override happens
via registration, not subclassing. Protocol built-ins fixed by ADR 0018 and
ADR 0023 are the exception: their validation and mutation cannot be replaced,
and a notification registration for one of those methods adds a
post-validation user hook. When the built-in mutates state, that hook observes
the mutation.
_Avoid_: Custom handler (less precise), override (the mechanism, not the
thing being registered).

**Server**:
The object (`Server<S>`) that owns exactly one LSP connection and its
initialization lifecycle, built by `Server::builder(state)` after static
registrations and served through a [[Transport]] constructor such as
`lspf::stdio(server)`. A second connection requires a second `Server`;
connection state is never shared between servers. Defined by ADR 0017.
_Avoid_: Session, backend, dispatcher (the pre-0.2 concept).

**Client**:
The object (`Client`) that owns one outbound LSP connection's initialization
inputs, reverse-request registrations, and caller-provided [[Transport]].
`Client::connect` establishes it and returns one [[ClientConnection]].
_Avoid_: Editor (the caller owns editor policy), connection (the connected state
is `ClientConnection`).

**ClientConnection**:
The initialized, exclusively owned client endpoint returned by
`Client::connect`; it drives incoming messages and exposes a cloneable
[[ServerHandle]] until its [[Transport]] ends.
_Avoid_: Session (the protocol session is private), ClientHandle (the opposite
message direction).

**Protocol session**:
The private connection core shared by Server and Client endpoints for correlation, bounded admission and queues, deadlines, task ownership, writer coordination, and idempotent close. Endpoint lifecycle, registration, and domain-state policy remain outside it.
_Avoid_: Endpoint, engine (those own direction-specific policy).

**Resource policy**:
The single connection-owned value that declares finite budgets for admitted
inbound requests, queued outbound messages and bytes, tracked Documents and
their text, and request and handler deadlines. Inbound request admission occurs
before parameter decoding, cancellation-token allocation, registry insertion,
and handler-task creation. Once the budget is full, a unique request receives
`ServerCancelled` (`-32802`, `inbound request capacity exhausted`) without
entering those structures; a duplicate ID remains `InvalidRequest` and never
replaces the admitted request. Completion, peer cancellation, and connection
close release registry ownership immediately; the admission permit remains
attached to admitted handler tasks until completion or abort and is released
when the [[Protocol session]] reaps the finished task handle.
Outbound admission charges one message and its exact encoded JSON-RPC envelope
bytes until the transport attempt finishes. Ordinary `ClientHandle` and
`ServerHandle` sends fail with `ClientError::OutboundOverloaded` when either
budget is full. Required
responses, protocol errors, and `$/cancelRequest` use the connection's single
failure-close path if admission fails; normal close stops admission and drains
the already-accounted queue.
A Layer may replace the policy's finite handler timeout for one inbound
request before forwarding it.
_Avoid_: Concurrency limit (only one budget), resource options (does not express
the enforced contract).

**Router**:
The permanently frozen table (`Router<S>`) of user request, notification,
and [[Command]] handlers for one connection, plus the capability catalog
those registrations imply (ADR 0017). Registrations commit through the
static builder and the single `configure_initialize` transaction; the
Server protocol engine then freezes the Router forever and computes
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
the Server protocol engine's built-in document-sync handlers; user code reads it
through a read-only `DocumentsView` from the [[ServerContext]] parameter
(`ctx.documents()`), and post-mutation hooks observe the updated state.
Identity is one normalized URI key (ADR 0021): equivalent spellings —
scheme and host case, percent-encoding, Windows drive-letter case —
address one [[Document]], while public values keep the client's original
URI.
_Avoid_: Document store (correct but verbose).

**Workspace**:
The cloneable handle to the connection's workspace state, exposed
through [[ServerContext]] (ADR 0017). The Server protocol engine establishes it from
`InitializeParams` during initialization — client info, capabilities,
initialization options, root URI, and workspace folders, all verbatim
and order-preserving (ADR 0021) — and owns its later mutation
(`workspace/didChangeWorkspaceFolders`), latest raw configuration settings,
and trace level; user hooks observe post-mutation state. It owns the
connection's [[Documents]] handle, so the read-only
`DocumentsView` user code sees comes from it. `roots()` prefers the
announced folders and falls back to one synthetic root derived from
`rootUri`, named for its final path segment or `"workspace"`.
_Avoid_: Project, root (the LSP `rootUri` is only an input to it).

**ClientHandle**:
The cloneable typed handle for server-to-client requests and notifications,
exposed through [[ServerContext]] (`ctx.client()`). A typed notification is
encoded and enqueued without allocating an ID; a typed request reserves a
connection-local, never-reused ID and awaits its correlated response
(ADR 0018). A finite default deadline bounds each request unless the resource
policy disables it; expiry returns `ClientError::Timeout`, releases the pending
request, and attempts one `$/cancelRequest`. Abandonment has the same
cancellation behavior, late responses are ignored because IDs are never
reused, and session close resolves every pending request with
`ClientError::Cancelled`.
Named helpers cover the stable outgoing notification surface
(`publish_diagnostics`, `show_message`, `log_message`, `log_trace`,
`telemetry_event`, `progress`): each sends its LSP-typed params verbatim and
returns enqueue failures as `ClientError`; `log_trace` gates on the
connection's shared trace level, enqueueing nothing while the level is `Off`.
Named request helpers cover the standard window and workspace interactions —
`show_document` (`window/showDocument`), `show_message_request`
(`window/showMessageRequest`), and `apply_edit` (`workspace/applyEdit`) —
each a thin wrapper over the typed request broker: the native LSP params
and results pass through verbatim under the broker's deterministic
completion semantics, and no helper adds UI, message-selection, or edit
policy. The client-owned workspace queries — `configuration`
(`workspace/configuration`) and `workspace_folders`
(`workspace/workspaceFolders`) — follow the same contract, and their
results go to the caller only: a query result never writes into the
Workspace configuration snapshot or the Workspace folder list, which
stay under protocol notification sync.
The dynamic capability announcements — `register_capability`
(`client/registerCapability`) and `unregister_capability`
(`client/unregisterCapability`) — tell the client about capability
changes while leaving the permanently frozen [[Router]] and the computed
initialize capabilities untouched; the framework retains no second list
of currently registered client capabilities, and any local route must
already exist through static or initialize-conditional registration.
The five stable workspace refresh helpers — `code_lens_refresh`
(`workspace/codeLens/refresh`), `diagnostic_refresh`
(`workspace/diagnostic/refresh`), `inlay_hint_refresh`
(`workspace/inlayHint/refresh`), `inline_value_refresh`
(`workspace/inlineValue/refresh`), and `semantic_tokens_refresh`
(`workspace/semanticTokens/refresh`) — each take no parameters and
return the client's `null` acknowledgement as `()`; they own no
recomputation policy, and the framework keeps no lens, diagnostic, hint,
value, or token state for them to touch. With the non-default `proposed`
Cargo feature, `refresh_folding_ranges` (`workspace/foldingRange/refresh`)
and `refresh_text_document_content`
(`workspace/textDocumentContent/refresh`, params naming only the target
document URI) join the refresh surface, using request markers and params
from the feature-gated `proposed` module because `lsp-types` 0.97.x lacks
them. The default refresh surface contains no proposed, draft, or notebook
method.
`begin_progress(ProgressOptions)` runs the connection-scoped work-done
progress lifecycle as one failure-safe operation: it allocates a
connection-local numeric token from a monotonic sequence (starting at 1,
skipping tokens already active on the connection, independent of the
outbound request-ID sequence), completes `window/workDoneProgress/create`,
registers the token only after the remote success, and enqueues exactly one
work-done begin notification. The returned `ProgressHandle` exposes its
`ProgressToken` and `CancellationToken`; `report` sends the exact work-done
report shape with percentages validated from 0 through 100, and `end`
consumes the handle, enqueues one work-done end, and removes the token
whether the enqueue succeeded or failed. A failed begin leaves no
registered token behind, and dropping an active handle removes its token
with a warning but performs no I/O and sends no implicit end.
`window/workDoneProgress/cancel` is a protocol-owned built-in against the
same connection-local registry: a matching active and cancellable token
fires the handle's `CancellationToken` without sending a work-done end —
the application decides the final message and calls `end` — while unknown,
malformed, ended, and non-cancellable tokens are logged at debug level and
ignored; a registered notification hook runs after a successful decode and
observes the updated cancellation state. Session close clears the registry,
so a handle that outlives the connection observes an unknown token.
`ClientHandle` is only a handle — the outbound queue, ID allocator, and pending
registry are owned by the connection's [[Protocol session]] and covered by the
connection's [[Resource policy]]. `ClientHandle` itself neither owns nor configures
those resources. An ordinary send that would exceed the policy returns
`ClientError::OutboundOverloaded` without retaining the message or leaking a
pending request entry.
_Avoid_: Connection (that is the transport level), sender.

**ServerHandle**:
The cloneable typed handle for client-to-server requests and notifications,
obtained from [[ClientConnection]]. It uses the connection's shared correlation,
outbound admission, deadline, cancellation, and close machinery.
_Avoid_: ClientHandle (the opposite message direction), connection (the handle
does not own or drive one).

**Command**:
A user-registered async closure dispatched on `workspace/executeCommand`
by name. Commands are how the user exposes custom actions to the editor
(refactorings, code generators, etc.) without inventing a new LSP method.
Distinct from a [[Handler]] in that one [[Handler]] (the built-in for
`workspace/executeCommand`) routes to many commands by string key.
_Avoid_: Action (LSP uses "code action" for something different),
custom request (a separate extension mechanism, see below).

**ServerContext**:
The cheap-to-clone framework-state handle passed by value to every
[[Handler]] (ADR 0017, revising ADR 0009's borrowed `&ServerContext`). Through
it handlers reach the established [[Workspace]] — initialization
metadata, roots, workspace folders, and the read-only [[Documents]]
view — and the [[ClientHandle]] for outgoing requests and
notifications, plus the current request's scope (id, tracing span).
It is the only way a handler reaches framework state — the user's own
struct holds only user-owned state, and user code never constructs a
`ServerContext`.
_Avoid_: Session, server-state.

**Transport**:
The message-framed channel over which LSP JSON-RPC envelopes flow into
and out of the framework. v1 ships four: stdio, TCP, WebSocket (all
native + tokio), and worker-channel (WASM in a browser or Node Worker,
wrapping a JS `MessagePort`). The trait sees one envelope at a time; framing
(`Content-Length` for stdio/TCP, none for the others) is the adapter's
concern.
_Avoid_: Connection (overloaded with TCP-specific meaning), socket
(byte-stream connotation), channel.

**Outcome**:
How one connection ended, returned by `Server::serve` over a [[Transport]]
(ADR 0018). Reader EOF, a writer failure, `exit`, and a fatal initialize
failure all converge on the [[Protocol session]]'s single close operation; the
first ordinary cause to arrive becomes the reported `Outcome`. Required
outbound admission failure is the exception: if it occurs before close
quiesces, the result is `WriterFailed` even when another cause arrived first
(ADR 0026). The outcome also carries the LSP exit code (0 only after a
successful `shutdown`). Serving returns the `Outcome` rather than terminating
the process — mapping it to a process disposition belongs to the server binary,
and `lspf::stdio(server).serve()` reports the same `Outcome` as
`Server::serve` over any other [[Transport]].
_Avoid_: Exit code (only one part of it), close reason, status.

**Runtime**:
The internal, crate-private trait through which the framework spawns and
cancels tasks (ADR 0020). Exactly two implementations exist —
`TokioRuntime` on native targets and `WasmRuntime` on browser or Node Worker
WASM —
selected by compile target, with no runtime-selection API. `Runtime`
executes spawn, abort, join, cooperative yield, and deadline sleep at the
[[Protocol session]]'s request but owns no protocol state; it is not
implementable or nameable by users.
_Avoid_: Executor, reactor (those are what `Runtime` delegates to).

**Layer**:
A framework-defined wrapper around a [[Service]] that adds cross-cutting
behavior to user dispatch (rate limits, audit logging, …). User Layers
are registered with `.layer(...)` and wrap only the user Service — the
last registered is outermost — while panic isolation, tracing, and
concurrency limiting are fixed framework-owned Layers outside them
(ADR 0019). Layers see decoded `IncomingCall` / `ServiceResult` values and may
override an inbound request's finite handler timeout before forwarding it.
They never see transport bytes and cannot intercept lifecycle, cancellation,
or document mutation, which are protocol built-ins. This is lspf's own
trait — narrower than `tower::Layer` and intentionally not interoperable
with it.
_Avoid_: Middleware (less precise; "layer" is the trait name and the
canonical term), interceptor.

**Error hook**:
The connection-level observer for framing, protocol, Transport,
panic-isolation, overload, and close failures. It receives only stable
categories and non-sensitive connection identity, runs outside user Layers,
and cannot alter responses, cleanup, or the connection outcome (ADR 0027).
_Avoid_: Error Layer, failure handler (it observes rather than handles).

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
