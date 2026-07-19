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
A handler that the framework ships out of the box. Initial scope: LSP
lifecycle (`initialize`, `initialized`, `shutdown`, `exit`) and text-document
sync (`didOpen`, `didChange`, `didClose`, `didSave`).
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

**Document**:
A text resource the framework tracks on behalf of the user, kept in sync
with the editor through `textDocument/didOpen`, `didChange`, `didClose`,
and `didSave`. Identified by URI; carries language ID, version, and
contents.
_Avoid_: File (a document may have no on-disk file), buffer (editor-side
term).

**Documents**:
The framework-provided, concurrency-safe handle to all tracked
[[Document]]s. Available on every [[Handler]] receiver through
`self.documents()`. Users read from it freely; mutations happen only
through the built-in document-sync handlers (unless explicitly
overridden).
_Avoid_: Workspace (broader concept in LSP), document store (correct but
verbose).

**Command**:
A user-registered async closure dispatched on `workspace/executeCommand`
by name. Commands are how the user exposes custom actions to the editor
(refactorings, code generators, etc.) without inventing a new LSP method.
Distinct from a [[Handler]] in that one [[Handler]] (the built-in for
`workspace/executeCommand`) routes to many commands by string key.
_Avoid_: Action (LSP uses "code action" for something different),
custom request (a separate extension mechanism, see below).

**Context**:
The framework-state handle passed to every [[Handler]] as a parameter.
Carries the [[Documents]] store, the workspace-folder cache, every
outgoing helper (`publish_diagnostics`, `show_message`, `apply_edit`,
…), and the current request's scope (id, tracing span). It is the only
way a handler reaches framework state — the user's own struct holds
only user-owned state.
_Avoid_: Client (the LSP-client direction is a sub-concern of
`Context`, not its name), session, server-state.

**Transport**:
The message-framed channel over which LSP JSON-RPC envelopes flow into
and out of the framework. v1 ships four: stdio, TCP, WebSocket (all
native + tokio), and worker-channel (WASM in browser, wrapping a JS
`MessagePort`). The trait sees one envelope at a time; framing
(`Content-Length` for stdio/TCP, none for the others) is the adapter's
concern.
_Avoid_: Connection (overloaded with TCP-specific meaning), socket
(byte-stream connotation), channel.

**Layer**:
A framework-defined wrapper around a [[Service]] that adds cross-cutting
behavior (panic catching, rate limits, audit logging, …). Composable
with other layers. This is lspf's own trait — narrower than
`tower::Layer` and intentionally not interoperable with it.
_Avoid_: Middleware (less precise; "layer" is the trait name and the
canonical term), interceptor.

**Service**:
The internal abstraction the dispatcher and every [[Layer]] implement.
Handles one request → response or one notification → unit at a time.
Users normally never see `Service` directly — their `LanguageServer`
impl is adapted into a `Service` by the framework.

**Default stack**:
The built-in set of [[Layer]]s installed by `lspf::stdio()`, `tcp()`,
`websocket()`, and `worker_channel()` when no explicit layer
configuration is provided. Currently: lifecycle, panic catching,
`$/cancelRequest` routing, bounded concurrency (64 in-flight by
default), and `tracing` spans. Users opt out with
`.no_default_layers()`.
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
