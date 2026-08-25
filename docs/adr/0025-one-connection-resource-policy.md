# One connection ResourcePolicy

Status: Accepted. Supersedes ADR 0012's concurrency-only resource assumption,
ADR 0015's acceptance of unbounded outbound growth, and the passages in ADR
0018 and ADR 0020 that retain an unbounded outbound queue.

One `Server` owns one `ResourcePolicy` covering every connection-owned growth
axis: admitted inbound requests; queued outbound messages and encoded bytes;
tracked Document count and text bytes; outbound-request deadlines; and handler
deadlines. Keeping these budgets in one value makes the connection's resource
contract visible and prevents independent builder knobs from producing hidden
or contradictory policy. The production defaults are finite: 64 inbound
requests, 1,024 outbound messages, 16 MiB of outbound bytes, 1,024 Documents,
64 MiB of Document text, and 30 seconds for each deadline. Only the outbound
request deadline may be explicitly disabled because some peers legitimately
cannot promise a response interval; handler work remains bounded by default.

`ServerBuilder::build` validates the complete policy and rejects every zero
budget and every enabled zero deadline. We considered exposing only `NonZero*`
fields, but build-time validation keeps struct-update configuration ergonomic
and gives all endpoint-construction failures one established error path.

Enforcement lands at the owning seams: admission before handler task creation,
outbound queue accounting at encode/enqueue, Document accounting at protocol
mutation, outbound expiry in the pending-request broker, and handler expiry at
the completion gate. Those changes are staged in issues #164 through #168;
ADR 0026 fixes the outbound admission and failure-close rules implemented by
issue #165. The remaining enforcement work stays staged in issues #166 through
#168.
