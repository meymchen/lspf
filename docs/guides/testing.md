# Protocol testing

Enable the native-only `testing` feature in development dependencies to run a
real lspf endpoint without sockets or child processes:

```toml
[dev-dependencies]
lspf = { version = "0.9.1", default-features = false, features = ["testing"] }
tokio = { version = "1", features = ["macros", "rt", "time"] }
```

`MemoryTransport::pair` returns the Transport supplied to the endpoint and a
`ScriptedPeer` retained by the test. Messages sent in either direction are
cloned into one `WireCapture`, whose zero-based sequence numbers preserve the
order in which traffic crossed the Transport seam.

```rust
use std::borrow::Cow;

use bytes::Bytes;
use lspf::testing::{MemoryTransport, WireDirection};
use lspf::{RawMessage, Transport, TransportReader, TransportWriter};

# async fn example() {
let (transport, mut peer) = MemoryTransport::pair();
let capture = peer.capture();
let (mut reader, mut writer) = transport.split();

peer.send(RawMessage::Notification {
    method: Cow::Borrowed("test/inbound"),
    params: Bytes::from_static(b"{}"),
}).unwrap();
assert_eq!(reader.recv().await.unwrap().method(), Some("test/inbound"));

writer.send(RawMessage::Notification {
    method: Cow::Borrowed("test/outbound"),
    params: Bytes::from_static(b"{}"),
}).await.unwrap();
assert_eq!(peer.recv().await.unwrap().method(), Some("test/outbound"));

let traffic = capture.snapshot();
assert_eq!(traffic[0].direction(), WireDirection::PeerToEndpoint);
assert_eq!(traffic[1].direction(), WireDirection::EndpointToPeer);
# }
```

`ServerJourney::start` drives a Server through initialize and initialized;
`finish` sends shutdown and exit and returns its `Outcome`. `ClientJourney`
does the symmetric job for a Client and exposes its `ServerHandle`. The
`start_with` variants accept non-default initialization values, and `peer()`
keeps custom requests, notifications, responses, and scripted Transport
failures under the test's control.

`VirtualClock::pause` controls the same Tokio clock that lspf uses for request
and handler deadlines. It must be created inside a current-thread Tokio
runtime. Call `advance` after the message that arms the deadline has appeared
at the scripted peer; the clock jump then makes the timeout deterministic.

## Repository concurrency model

The repository has a dependency-free model of the private protocol session in
`crates/lspf/tests/concurrency_model_support`. It checks every
order-preserving interleaving in six representative scenarios:

- independently tagged, out-of-order responses;
- completion racing cancellation and later capacity reuse;
- bounded queue producers racing sends and close;
- pending requests and owned tasks racing repeated close; and
- writer or required-message failure racing reader EOF.

The model asserts correlation and exactly-once completion, the two-message and
eight-byte model queue limits, complete charge and inbound-capacity release,
one close cleanup, joined task ownership, and writer-failure precedence before
quiescence. These small limits are model bounds, not production defaults.

Run all scenarios with the ordinary integration-test command:

```console
cargo test -p lspf --test concurrency_model --no-default-features
```

The test is part of Cargo's automatic integration-test discovery, so the
workspace CI suite runs it without a separate job. A failure includes the
scenario name followed by the exact actor and operation sequence. The search
order is deterministic; rerunning the named test reproduces the same failing
trace.
