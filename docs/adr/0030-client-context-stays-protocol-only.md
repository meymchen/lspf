# ClientContext stays protocol-only

Status: Accepted.

Client reverse handlers receive a `ClientContext` containing their call scope
and the connection's `ServerHandle`, allowing nested typed calls without
constructing an editor abstraction. Workspace, UI, filesystem, extension-host,
progress presentation, and dynamic-registration policy remain caller-owned;
the framework only decodes and dispatches those typed protocol messages through
connection-local handlers and the shared protocol session. Work-done progress
tokens are protocol state, so each Client connection validates their one
create-or-request registration, begin, and end lifecycle in its own registry
without retaining any presentation state. Handler delivery is serialized per progress token so wire
order remains observable even when one handler awaits a nested reverse call;
different tokens and the connection read loop remain concurrent.
