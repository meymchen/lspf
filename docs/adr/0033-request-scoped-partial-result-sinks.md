# Keep partial-result sinks request-scoped and budgeted

Status: Accepted.

A partial-result sink is borrowed from the `ServerContext` for one request. It
exists only when that request carried a `partialResultToken`, its typed marker
matches the method being handled, and the vendored LSP metaModel marks the
method as supporting partial results. A synchronized request-lifetime gate
rejects reports after the handler completes, including reports attempted from
a cloned context. Dropping the sink performs no I/O and needs no explicit
finish message; the request's normal response ends the stream.

Each report is an ordinary `$/progress` notification admitted through the
connection's existing outbound message and exact-byte budgets. Reports keep
call order because they enter the same FIFO as the response. A full budget
returns `ClientError::OutboundOverloaded` to the reporting handler and retains
no part of the rejected chunk. We rejected a separate streaming queue and
unbounded or silently lossy reporting because each would let a chatty handler
evade the connection's resource policy. We also rejected waiting for transport
capacity: outbound admission is deliberately synchronous and does not hold a
handler on peer I/O.
