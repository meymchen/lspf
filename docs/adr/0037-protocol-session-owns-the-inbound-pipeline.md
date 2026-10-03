# Protocol session owns the inbound pipeline

Status: Accepted. Revises ADR 0028.

The [Protocol session](0028-shared-private-protocol-session.md) owns every
inbound step that does not depend on endpoint policy: the read loop, Transport
error classification, admission and its rejections, `$/cancelRequest` claims,
response correlation, handler spawn with deadline and panic backstop, failure
reports and telemetry for those steps, and close-cause selection. Endpoints pull
events from it and see only admitted requests and notifications the session does
not own. Each admitted request must be answered exactly once, by responding
inline or by spawning work the session completes.

Before this decision both endpoints assembled the pipeline from session
primitives, and the copies drifted: the Client skipped rejection telemetry and
silently lost notification-handler panics. Where the endpoints differed, the
Server's behaviour now applies to both. The session's panic catch is a backstop
for every task it spawns; the Server's fixed panic-isolation Layer from ADR 0019
stays in place.

A pull interface was chosen over an endpoint callback trait because the Client's
initialize phase races its pending response against inbound events, and the
Server's dispatch awaits lifecycle hooks on the read loop while mutating
endpoint state. Both compose with a pull loop without an async trait or
per-phase endpoint state.
