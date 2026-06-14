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

## Addendum (2026-06-15): document-sync mutations are inline too

The first addendum claimed every feature method, "hover, completion,
didOpen, …", gains nothing from ordered execution. That is wrong for the
document-sync notifications. The built-in state mutation for
`textDocument/didOpen`, `didChange`, `didClose`, and `didSave` runs
**inline** in the read-loop, in receipt order, so the mutation lands in
the [[Documents]] store before the next message is dispatched. The
inline category is therefore lifecycle **plus** document-sync mutation,
not lifecycle alone. Requests still spawn (ADR 0012), and user-side
reactions to a change (publishing diagnostics, reparsing) spawn too —
only the built-in rope mutation is inline.

This is the ordering constraint the LSP spec leaves implicit. The spec
(3.18, *Request, Notification and Response Ordering*) lets a server
reorder messages "as long as this reordering doesn't affect the
correctness of the responses." A reordered `didChange` crosses that
line: incremental edits applied out of order corrupt the rope, and a
later request then reads a broken document.

We rejected **spawning the mutation** (the model lspf shipped through
0.1.0-alpha.2, and tower-lsp's default). `tokio::spawn` hands the
mutation to the scheduler with no ordering guarantee relative to the next
`didChange` or a following request, so two edits can land out of order.
tower-lsp's maintainer (issue #284) and async-lsp both classify this as a
spec violation — the server and client states "slowly drift apart."

We rejected a **per-document serial queue** (`HashMap<Uri, mpsc>`, FIFO
per URI, parallel across URIs). It is more machinery than either
reference implementation uses, and it optimizes the wrong cost: a rope
mutation is microseconds, so running it inline does not meaningfully
stall the read-loop, while the expensive work (reparse, diagnostics) is
user-side and spawns in *every* design. Its one distinctive benefit — a
slow mutation on document A not blocking document B — buys almost nothing
because the mutation is never the slow part, and to preserve
read-after-write it would force same-document requests to serialize
behind the queue, discarding most of the request concurrency the spawn
model exists for.

Both canonical implementations reach the same guarantee by different
mechanisms. Microsoft's `vscode-jsonrpc` is concurrent-by-default at the
connection layer (`maxParallelism` defaults to `-1`; it dispatches the
next queued message without awaiting the current handler), yet document
sync stays correct because the built-in `TextDocuments` listener is
**synchronous** — on a single-threaded event loop the mutation completes
within the synchronous prefix of dispatch, before the next message is
touched. async-lsp states the rule directly: "notifications must be
processed in order (synchronously) … requests can be processed
concurrently." Running the mutation inline in our read-loop is that same
guarantee expressed for a multi-threaded tokio runtime, where `spawn`
would otherwise throw it away.

The cost we accept: the read-loop is blocked for the duration of a
built-in document-sync mutation. This is bounded and cheap by
construction (the built-in does no user work), and it mirrors the slow-
`initialize` cost above. A user who overrides a document-sync handler
with an expensive body reintroduces the stall; the built-ins keep their
reaction work (diagnostics) on a spawned task, and overrides are
documented to do the same.
