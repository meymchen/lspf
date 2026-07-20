# Runtime and the native/WASM Send model

Status: Accepted. Fixes the interface of the `Runtime` ownership boundary
named by [ADR 0017](0017-typed-router-and-capability-catalog.md) and the
execution seam for the task group and close path that `ProtocolEngine` owns
per [ADR 0018](0018-protocol-engine-and-outbound-request-broker.md).
Retains the Transport shape of
[ADR 0011](0011-transport-shape-and-v1-adapters.md) while updating its
execution constraints. Concretizes the per-target executors chosen in
[ADR 0001](0001-async-only-runtime.md).

## Context

ADR 0001 makes lspf async-only, running on tokio natively and
`wasm-bindgen-futures` in browser WASM. ADR 0017 names `Runtime` as the owner
of task spawning, cancellation primitives, and target-specific execution, but
leaves its exact interface to a later ADR. ADR 0018 gives `ProtocolEngine`
exclusive ownership of the connection task group and the cancel-then-join
close path, with a `Runtime` executing engine-requested spawning without
owning the group or its cancellation policy.

No earlier ADR fixes how one protocol kernel executes on two targets with
incompatible threading models. Native tokio is multi-threaded: a spawned task
may migrate between worker threads, so its future must be `Send`.
`wasm32-unknown-unknown` inside a Web Worker is single-threaded:
`spawn_local` never moves a future across threads, and the JS handles behind
the worker-channel adapter of ADR 0011 — most importantly the `MessagePort` —
are thread-affine and cannot soundly be made `Send`.

Left open, every implementation milestone would have to invent its own answer
to where `tokio::spawn` may appear, which bounds handler registration carries
on each target, and how a non-`Send` transport coexists with a `Send`-bounded
trait. ADR 0017 explicitly defers the WASM relaxation of its
`Send + Sync + 'static` registration bounds to this decision. This ADR locks
that model with one solution per question.

## Decision

The protocol kernel — `ProtocolEngine`, `RouterService`, the `Service` and
`Layer` stack, the outbound request broker, and the send loop — never calls
`tokio::spawn`, `tokio::task::spawn_local`, or
`wasm_bindgen_futures::spawn_local` directly. Every spawn goes through the
internal `Runtime` adapter.

`Runtime` is an internal, crate-private trait. It is not implementable by
normal framework users and is not a registration surface. Exactly two
implementations ship: `TokioRuntime` on native targets, delegating to
tokio's spawn, and `WasmRuntime` on `wasm32-unknown-unknown`, delegating to
`wasm_bindgen_futures::spawn_local`. Selection is fixed by compile target
through ADR 0013's `tokio` and `wasm` feature glue; there is no runtime
selection API, and any deterministic test runtime used by engine tests
stays behind the crate boundary.

The framework defines one internal conditional marker trait, `TaskSend`,
that carries the entire native/WASM `Send` difference. On native targets
`TaskSend` has the `Send` supertrait and every `Send` type satisfies it. On
`wasm32` it has no supertrait and every `'static` type satisfies it. Spawned
futures are bounded by `TaskSend + 'static`. Users cannot implement
`TaskSend` manually; it is satisfied automatically or not at all.

Public handler registration names, parameters, and return shapes are
identical on native and WASM. The ADR 0017 builder methods and typed handler
signatures do not gain target-specific variants; only the internal task and
handler bounds differ, expressed through `TaskSend`. The business call shape
of ADR 0017 is unchanged.

Transport reader and writer execution requires `Send` on native: the
`Transport` trait keeps its `Send + 'static` supertrait there, and the reader
and writer tasks are ordinary `Send` futures on the multi-threaded runtime.
On `wasm32` the `Send` supertrait is dropped and the connection's tasks run
on the worker's single thread. The framework never writes
`unsafe impl Send` or `unsafe impl Sync` for JS-backed types; WASM must not
fake `Send`.

Task cancellation is cooperative on both targets. It uses the task handle's
abort operation and the session and request `CancellationToken`s from
ADRs 0007 and 0018, taking effect at the next yield or check point. No part
of the framework relies on forced thread preemption, thread termination, or
Worker termination to stop a task.

The kernel's channel, semaphore, and task-bookkeeping primitives — the
inbound and outbound registries, the task group, the ADR 0012 concurrency
limit, and the ADR 0015 outbound queue — must compile on
`wasm32-unknown-unknown`. Tokio is confined to `TokioRuntime` and the native
Transport adapters. M0 does not fix the exact third-party crate for each
primitive; before the 0.5 milestone, a minimal compile spike must build the
chosen primitives for `wasm32-unknown-unknown`, and a primitive that fails
the spike is replaced rather than weakening this boundary.

## Interface and behavior

### The TaskSend conditional marker

The semantic definition is:

```rust
#[cfg(not(target_arch = "wasm32"))]
pub trait TaskSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> TaskSend for T {}

#[cfg(target_arch = "wasm32")]
pub trait TaskSend {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> TaskSend for T {}
```

As in ADR 0017, purely mechanical Rust disambiguation may add syntax but may
not change these semantics. The `wasm32` blanket impl covers every type; the
`'static` half of the rule is carried by the spawn and registration bounds,
which always pair the marker as `TaskSend + 'static`. `TaskSend` may appear in public bounds so that
handler registration compiles on both targets from one source, but it is
sealed against manual implementation and is not a user extension point. A
type either already satisfies the target's requirement or the program does
not compile; there is no opt-in or opt-out.

ADR 0017's registration bounds are now target-conditional through this
marker. On native targets they remain exactly `Send + Sync + 'static` for
handlers and `Send + 'static` for their returned futures. On `wasm32` they
reduce to `'static`. Handler names, parameter lists, and return types are
byte-for-byte identical across targets.

### Runtime and task handles

The internal `Runtime` seam has this semantic shape:

```rust
pub(crate) trait Runtime {
    fn spawn(
        &self,
        fut: impl Future<Output = ()> + TaskSend + 'static,
    ) -> TaskHandle;
}
```

`TaskHandle` provides two operations: `abort`, which requests cooperative
cancellation of the task, and an awaitable join that resolves once the task
has completed or its aborted future has been dropped. `ProtocolEngine`
records every `TaskHandle` in its engine-owned task group; the ADR 0018
close operation aborts and then joins through those handles.

`TokioRuntime` maps `spawn` to tokio's spawn and `TaskHandle` to tokio's
join and abort handles. `WasmRuntime` maps `spawn` to
`wasm_bindgen_futures::spawn_local`, wrapping the future so that `abort`
causes it to be dropped at its next yield point and the join resolves
afterward. On both targets abort is observed at a yield point; neither
implementation preempts a running compute section, which is why CPU-bound
handlers poll `CancellationToken::is_cancelled` per ADR 0007.

`ProtocolEngine` remains the sole owner of the task group, the session
cancellation token, and the cancel-then-join close policy fixed by ADR 0018.
`Runtime` executes spawning and exposes abort and join; it does not decide
when to spawn, cancel, or close, and it holds no protocol state.

### Target execution models

On native targets, `TokioRuntime` spawns onto the multi-threaded tokio
runtime. Every spawned future, including the reader and writer tasks and
every dispatched handler future, is `Send + 'static`. The `Transport` trait
keeps the ADR 0011 `Send + 'static` supertrait, and the stdio, TCP, and
WebSocket adapters remain tokio-based.

On `wasm32-unknown-unknown`, one Web Worker runs the whole connection.
`WasmRuntime::spawn` delegates to `spawn_local`; futures interleave at await
points on the single thread and there is no parallelism. The `Transport`
trait drops the `Send` supertrait on this target while keeping the same
three methods, so the worker-channel adapter holds its `MessagePort` without
pretending to be `Send`. The ADR 0012 concurrency limit still bounds
in-flight handler tasks; it limits interleaved tasks rather than parallel
ones.

Because the WASM bounds are strictly weaker, handler code written for native
compiles unchanged for WASM. The reverse is not guaranteed: a handler that
holds an `Rc` or a JS value across an await compiles only on `wasm32`.
Applications targeting both compile against the native bounds.

### Portable primitives and the compile spike

The kernel's synchronization and bookkeeping primitives are chosen for
target portability, not per-target duplication. Channels (including the
ADR 0015 unbounded outbound queue), the concurrency semaphore, both request
registries, and the task group's bookkeeping must compile for
`wasm32-unknown-unknown` with the `tokio` feature disabled. Tokio's
executor, I/O, and time facilities appear only inside `TokioRuntime` and the
native Transport adapters.

M0 may implement against provisional primitives without fixing every crate
choice. Before the 0.5 milestone, a minimal compile spike — building the
kernel and its chosen primitives for `wasm32-unknown-unknown` — must pass.
A primitive that cannot pass is replaced with one that can; the spike
failing is never grounds for reintroducing tokio into the kernel or faking
`Send`.

## Rejected alternatives

We rejected calling `tokio::spawn` and `spawn_local` directly from the
kernel behind scattered `cfg` blocks. That couples every spawn site to both
executors, makes the close path's cancel-then-join guarantee depend on
duplicated per-target code, and leaves no seam for a deterministic test
runtime.

We rejected a public, user-implementable `Runtime` trait. The ADR 0018
invariants — one close path, cancel-then-join, no detached tasks — depend on
the runtime's abort and join semantics, and an arbitrary user executor could
silently break them. Normal framework users choose a transport constructor,
not an executor.

We rejected `unsafe impl Send` wrappers around `MessagePort` or other
JS-backed types to preserve one unconditional `Send` bound. JS values are
thread-affine; such an impl is unsound the moment any future actually
crosses a thread, and it would hide the real difference this ADR exists to
manage.

We rejected making the whole framework non-`Send` and running native servers
on a current-thread executor. That would surrender multi-core execution of
concurrent handlers on the primary target to simplify the secondary one,
inverting the project's performance goal and contradicting the concurrent
dispatch model of ADR 0003.

We rejected split public APIs — differently named or differently shaped
registration per target. ADR 0017 fixes one call shape; two vocabularies
would force every downstream example, test, and application to fork on
target.

We rejected preemptive cancellation: killing native threads or terminating
the Worker to stop a task. Thread termination corrupts shared state,
terminating the Worker destroys the whole connection rather than one task,
and ADR 0007 already fixes cooperative cancellation as the model.

We rejected running tokio's executor on `wasm32`. The browser event loop
must remain the driver inside a Worker, tokio's I/O and time drivers do not
function there, and `wasm-bindgen-futures` already bridges Rust futures onto
the JS microtask queue.

## Consequences

The protocol kernel is target-agnostic: it spawns through one seam, bounds
tasks by one marker, and never names an executor. The cost is one level of
indirection on every spawn and an internal trait contributors must route new
task creation through. The seam also permits an internal deterministic test
runtime for engine tests without making executors a public feature.

Contributors reason about `TaskSend` instead of raw `Send` in kernel bounds,
which is unfamiliar. In exchange, users see a single documented API whose
programs compile identically on both targets, and the WASM build cannot
accidentally acquire a `Send` requirement it cannot meet.

Portability is asymmetric by design: native-valid handler code is always
WASM-valid, while WASM-only handler code may not compile natively. Teams
targeting both build against native bounds; teams targeting only the browser
gain the freedom to hold non-`Send` values across awaits.

Custom executors (async-std, smol, embedded runtimes) are not supported in
v1. Applications with such constraints run the native tokio path or build
outside this framework boundary.

Deferring exact primitive crates keeps M0 unblocked, at the price of a hard
gate later: the pre-0.5 compile spike can force a primitive swap. Because
the kernel confines tokio behind the `Runtime` and adapter boundaries, such
a swap is contained and does not change public API.

## Migration impact

ADR 0011's Transport shape — message-framed `recv`, `send`, `shutdown`, and
the four v1 adapters — is retained. Its trait-level `Send + 'static`
supertrait now applies to native targets only; on `wasm32` the bound is
`'static`. ADR 0011 carries a status note pointing here; its historical body
is unchanged.

ADR 0017 is completed, not changed: the WASM relaxation it assigned to this
ADR is now fixed as the `TaskSend` marker, and its builder methods and typed
handler signatures stay exactly as written. ADR 0018 is likewise completed:
the `Runtime` it references as executing engine-requested spawning now has
its interface, its two implementations, and its non-ownership of the task
group locked.

Implementations and prototypes that call `tokio::spawn` inside dispatch,
broker, send-loop, or engine code must route those spawns through `Runtime`.
Kernel code that names tokio types for channels, semaphores, or task
bookkeeping must either confirm the chosen primitive compiles on
`wasm32-unknown-unknown` or move behind the native-only boundary. The
ADR 0015 outbound queue keeps its unbounded, dedicated-send-loop behavior,
but its concrete `tokio::sync::mpsc::unbounded_channel` selection is
superseded by the portability constraint: the implementing primitive must
pass the pre-0.5 spike. ADR 0015 carries a status note recording this.

ADR 0001's async-only decision and ADR 0007's cancellation model are
unchanged in substance; this ADR names their concrete executors
(`TokioRuntime`, `WasmRuntime`) and binds cancellation to `TaskHandle` abort
plus the existing tokens.

## Tests required by downstream milestones

The runtime, engine, transport, and WASM milestones must add tests proving:

- the kernel and its chosen channel, semaphore, and task-bookkeeping
  primitives compile for `wasm32-unknown-unknown` with the `tokio` feature
  disabled — the pre-0.5 compile spike, kept as a CI target thereafter;
- no direct `tokio::spawn`, `tokio::task::spawn_local`, or
  `wasm_bindgen_futures::spawn_local` call exists outside `TokioRuntime`,
  `WasmRuntime`, and the native Transport adapters, enforced by a structural
  lint or source check;
- one shared example source registers the same handlers with identical names
  and parameters and compiles for both native and `wasm32` targets;
- on native targets, spawned engine tasks and the reader and writer futures
  satisfy `Send` at compile time;
- `TaskSend` rejects manual downstream implementation (compile-fail) and
  `Runtime` is not nameable outside the crate;
- `TaskHandle::abort` stops a cooperative task at its next yield point on
  both runtimes and its join then resolves, without thread or Worker
  preemption;
- the ADR 0018 close operation performs cancel-then-join through `Runtime`
  task handles identically on `TokioRuntime` and `WasmRuntime`, detaching no
  task;
- request-scoped `CancellationToken`s reach handlers and report cancellation
  on both targets per ADR 0007; and
- no `unsafe impl Send` or `unsafe impl Sync` for JS-backed types exists in
  the `wasm32` build, enforced by lint or source check.
