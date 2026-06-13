# `&self` handlers with concurrent dispatch

`LanguageServer` trait methods take `&self`. The dispatcher holds the
user's server in an `Arc` and spawns each incoming request or
notification as its own task, so multiple handlers can be in flight at
once. Users put their own mutable state behind interior mutability
(`Mutex`, `RwLock`, atomics) inside their server struct.

We rejected `&mut self`: it forces sequential dispatch, which makes one
slow handler (e.g. formatting) block every other one (hover, completion),
defeating the async runtime. We rejected actor-style channels: LSP
traffic is read-heavy across all of hover, completion, semantic-tokens,
and diagnostics, and a single-owner actor cannot exploit that
parallelism. We rejected `Arc<Self>` as the explicit receiver: same
concurrency as `&self` with worse signature ergonomics on every method.

The cost we accept: users hit Rust's interior-mutability rules whenever
they want to mutate their server's own state. We absorb the most common
case — the [[Document]] store — by shipping a concurrency-safe
[[Documents]] handle on the trait, so the typical handler reads
documents through `self.documents()` without writing any lock code.

## Addendum (2026-06-14): explicit `Send` futures and lifecycle inline-vs-spawn split

For the dispatcher to `tokio::spawn` handlers against `Arc<S>`, every
trait-method future must be `Send`. Trait methods are declared as
`fn name(...) -> impl Future<Output = R> + Send` rather than
`async fn name(...) -> R`. User impls keep writing `async fn`
overrides; the compiler verifies the body is `Send`. This matches the
shape already used by the `Transport` trait (ADR 0011), removes the
`#[allow(async_fn_in_trait)]` suppression in `src/server.rs`, and adds
zero new dependencies.

We rejected leaving `async fn` in the trait: the
`async_fn_in_trait` future is not currently `Send` in the general case,
so the trait does not compose with `tokio::spawn`. We rejected the
`trait-variant` macro: it generates a Send-only variant from `async fn`
signatures, but the trait file *is* the user-facing contract here — a
proc-macro between the user and the signature they implement against
makes goto-def and rustdoc less direct for marginal compactness gain.
We rejected `async-trait`: it boxes every returned future, contradicting
ADR 0001's zero-overhead-async stance.

Lifecycle methods (`initialize`, `shutdown`, `exit`) run **inline** in
the read-loop — `server.initialize(&ctx, ...).await` without
`tokio::spawn`. All other methods spawn. This mirrors how the LSP spec
itself partitions lifecycle from feature methods: the read-order
invariants (initialize must complete before any other request,
shutdown's response must precede exit's process termination) require
synchronous progression of the dispatcher's state machine, while every
feature method (hover, completion, didOpen, …) gains nothing from
ordered execution.

The cost we accept here: a slow `initialize` handler blocks the
read-loop for its full duration. This is correct per the LSP spec —
clients are not permitted to send other requests until `initialize`
returns — and it surfaces the latency on the handler author rather
than hiding it behind a spawn boundary.
