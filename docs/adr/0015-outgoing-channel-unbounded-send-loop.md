# Outgoing channel: unbounded, drained by a dedicated send-loop task

[[Context]]'s outgoing helpers (`publish_diagnostics`, `show_message`,
`apply_edit`, …) push [[RawMessage]]s onto a single
`tokio::sync::mpsc::unbounded_channel`. A dedicated task, spawned by
the dispatcher at startup, owns the receiver and writes each message
to the [[Transport]]. The read-loop and the send-loop run concurrently
and never coordinate beyond the channel itself. The sync helpers on
[[Context]] keep their non-`async` signatures: a handler emits a
notification with one statement, no `.await`.

We rejected `tokio::sync::mpsc::channel(N)` paired with `async fn
publish_diagnostics(&self, ...)` (the tower-lsp shape): it propagates
back-pressure into handler code, which sounds clean but means a slow
or wedged LSP client can stall arbitrary handlers — including handlers
unrelated to the publish, because the channel is shared. It also
breaks the [[Context]] glossary entry's "fire-and-forget" framing of
the outgoing helpers and forces every call site to `.await`. We
rejected `try_send` with a drop-on-full fallback: dropping
`publishDiagnostics` frames is technically safe under LSP semantics
(the client always honours the latest version) but constitutes a
silent failure, and a saturated 256-slot channel is a signal that
*something* is wrong rather than a problem we should paper over by
losing data. We rejected the async-lsp shape (single `select!`-biased
loop, no dedicated send task): biasing the main loop toward outgoing
gives real read-side back-pressure, but it couples read and send onto
the same await point, and now that every handler runs on its own
`tokio::task` there is nothing for the main loop to do *except* read
and forward — so a dedicated send-task is the simpler decomposition.

The cost we accept: an LSP client that accepts the TCP/pipe but stops
draining the socket lets the outgoing channel grow without bound, and
the send-loop's `transport.send().await` will eventually block on the
OS buffer rather than on our channel. In the steady state this is a
non-issue — VS Code, neovim, helix, and every other mainstream LSP
client drain promptly — but a misbehaving client can grow our memory
footprint. We accept this in exchange for the API simplicity and for
keeping back-pressure semantics off the user-facing trait. If a real
client ever exhibits this pattern, we revisit by adding an
explicit-bound variant rather than retrofitting `.await` into
`publish_diagnostics`.

This decision is the outgoing-path counterpart to ADR 0003's spawn-per-
handler decision: ADR 0003 says "inbound work is concurrent across
handlers"; this ADR says "outbound work is concurrent with inbound, and
the channel between them does not impose back-pressure". ADR 0012's
"bounded concurrency by default" applies to the count of in-flight
handler tasks, not to the size of the outgoing buffer — different axis,
different control.
