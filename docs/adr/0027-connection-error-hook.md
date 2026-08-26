# Keep connection failure observation outside user Layers

Status: Accepted.

Connection failures span framing, protocol ownership, Transport I/O, fixed
panic isolation, resource admission, and close cleanup, while user Layers wrap
only decoded user dispatch. `ServerBuilder::on_error` therefore registers one
synchronous connection observer outside the Layer chain. It receives stable
categories and non-sensitive identity rather than error text or payloads, and
the framework catches observer panics so reporting cannot change a response,
cleanup, or the selected `Outcome`. An async hook was rejected because waiting
for user work in failure and close paths would make cleanup progress depend on
the observer; routing these events through Layers was rejected because it
would expose protocol and Transport responsibilities at the user-dispatch
boundary.
