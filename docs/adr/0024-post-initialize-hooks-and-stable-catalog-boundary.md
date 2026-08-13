# Post-initialize lifecycle hooks and the stable catalog boundary

Status: Accepted. Extends ADR 0018's lifecycle hooks and ADR 0017's capability
catalog.

ADR 0018 named three lifecycle hooks; this ADR adds the fourth, `on_initialized`,
and fixes the boundaries of the stable catalog the 0.3 PRD defines.

## `on_initialized`

`on_initialized` has the notification-handler state, `Context`, and params
shape and runs at most once, only after a successful initialize transaction:
when the client's `initialized` notification arrives while the connection is
running. It resolves to `()`, and it cannot register routes or contribute
capabilities. An `initialized` received before `initialize` or after
`shutdown` is ignored without consuming the hook; malformed params are
dropped. The hook's params decode accepts both wire spellings of the empty
`InitializedParams` — `{}` and JSON-RPC `null` — so a client that omits params
does not lose its hook.

## `on_exit` clarification

`on_exit` keeps ADR 0018's notification-handler shape — shared state and
`Context`, no `CancellationToken`, resolving to `()` — and `exit` carries no
parameters, so the typed hook receives none. It runs before the engine
computes the exit outcome; the outcome then derives from protocol-owned
lifecycle state alone (0 only after a successful `shutdown`), so the hook
cannot choose or replace it. An `exit` received before `initialize` has no
established `Workspace` to hand the hook, so it is skipped and the connection
closes with code 1.

`on_shutdown`, specified by ADR 0018, remains unimplemented and out of scope
for this catalog work.

## Stable catalog boundary

The default catalog is stable LSP 3.17 only: no notebook method and no
proposed or 3.18-draft method appears in the default catalog or the default
capabilities. Custom requests and notifications remain registrable beside the
whole catalog and contribute nothing to `ServerCapabilities`;
`$/cancelRequest` stays a protocol built-in, and `workspace/executeCommand`
stays owned by the Command registry.

One deterministic full-catalog test registers every stable feature, built-in
notification hook, and command in one server and pins the initialize
response's capability JSON byte-for-byte against a fixture, so any added,
renamed, or reordered capability field — or a descriptor whose contribution
drifted — breaks the test rather than silently changing what a client
receives.
