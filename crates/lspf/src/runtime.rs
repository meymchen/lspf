// Exactly one concrete runtime exists in these configurations. On wasm32 the
// `wasm` feature is enforced by `lib.rs`'s compile_error, so the target check
// alone is enough there; on native targets a runtime needs `runtime-tokio`.
use std::future::Future;
#[cfg(any(
    all(not(target_arch = "wasm32"), feature = "runtime-tokio"),
    target_arch = "wasm32",
))]
use std::time::Duration;

#[cfg(target_arch = "wasm32")]
use std::cell::Cell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

mod sealed {
    pub trait Sealed {}

    impl<T: ?Sized> Sealed for T {}
}

/// Target-dependent task mobility bound.
///
/// Native tasks can move between Tokio worker threads, whereas the
/// Worker-hosted WASM runtime keeps them on the thread that spawned them.
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub trait TaskSend: sealed::Sealed + Send {}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> TaskSend for T {}

#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub trait TaskSend: sealed::Sealed {}

#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> TaskSend for T {}

/// A type-erased future that preserves the target's task mobility bound.
///
/// This behavioral trait lets public and internal boxed-future aliases name
/// [`TaskSend`] once instead of spelling target-specific `dyn Future + Send`
/// forks at every erasure site.
#[doc(hidden)]
pub trait TaskFuture<T>: Future<Output = T> + TaskSend {}

impl<T, F> TaskFuture<T> for F where F: Future<Output = T> + TaskSend + ?Sized {}

/// Crate-private task execution boundary (ADR 0020).
///
/// The protocol kernel reaches an executor only through this trait: it owns
/// spawning, cooperative yielding, and — through the returned [`TaskHandle`] —
/// abort and join. It holds no protocol state and is never nameable by users.
/// It owns spawning, cooperative yielding, and deadline sleeps. It exists only
/// where a runtime does: native targets with `runtime-tokio`, or wasm32 (whose
/// `wasm` feature `lib.rs` enforces).
#[cfg(any(
    all(not(target_arch = "wasm32"), feature = "runtime-tokio"),
    target_arch = "wasm32",
))]
pub(crate) trait Runtime {
    fn spawn<F>(&self, future: F) -> TaskHandle
    where
        F: Future<Output = ()> + TaskSend + 'static;

    /// Yield execution back to the runtime so other tasks can run.
    ///
    /// The kernel reaches an executor-specific yield only through this seam.
    /// Today's kernel sites all await task joins or channel receives, which
    /// yield naturally, so the method is exercised by the runtime's own tests
    /// rather than by kernel call sites.
    #[allow(dead_code)]
    fn yield_now(&self) -> impl Future<Output = ()>;

    /// Wait for a connection-owned deadline without exposing the selected
    /// executor to protocol modules.
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()>;
}

/// Wait on the selected runtime's clock.
#[cfg(any(
    all(not(target_arch = "wasm32"), feature = "runtime-tokio"),
    target_arch = "wasm32",
))]
pub(crate) async fn sleep(duration: Duration) {
    default_runtime().sleep(duration).await;
}

/// Runtime selected for native targets, delegating to the Tokio runtime.
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]
#[derive(Default)]
pub(crate) struct TokioRuntime;

#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]
pub(crate) fn default_runtime() -> TokioRuntime {
    TokioRuntime
}

#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]
impl Runtime for TokioRuntime {
    fn spawn<F>(&self, future: F) -> TaskHandle
    where
        F: Future<Output = ()> + TaskSend + 'static,
    {
        TaskHandle(tokio::spawn(future))
    }

    async fn yield_now(&self) {
        tokio::task::yield_now().await;
    }

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Serving must not begin before the target's executor exists. On native
/// targets that means a Tokio runtime the caller started — the framework
/// never starts one implicitly (ADR 0020). On Worker-hosted WASM the host's
/// `wasm-bindgen` glue owns the executor and cannot be detected from Rust.
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]
pub(crate) fn ensure_runtime_available() -> crate::Result<()> {
    tokio::runtime::Handle::try_current()
        .map(|_| ())
        .map_err(|_| crate::Error::RuntimeRequired)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn ensure_runtime_available() -> crate::Result<()> {
    Ok(())
}

/// Runtime selected for Worker-hosted WASM targets, delegating to
/// `wasm_bindgen_futures::spawn_local` on the worker's single thread.
#[cfg(target_arch = "wasm32")]
#[derive(Default)]
pub(crate) struct WasmRuntime;

#[cfg(target_arch = "wasm32")]
pub(crate) fn default_runtime() -> WasmRuntime {
    WasmRuntime
}

#[cfg(target_arch = "wasm32")]
impl Runtime for WasmRuntime {
    fn spawn<F>(&self, future: F) -> TaskHandle
    where
        F: Future<Output = ()> + TaskSend + 'static,
    {
        use futures_util::future::{AbortHandle, Abortable, FutureExt};

        let (abort, registration) = AbortHandle::new_pair();
        let (completed_tx, completed_rx) = futures_channel::oneshot::channel();
        let finished = Rc::new(Cell::new(false));
        let finished_for_task = Rc::clone(&finished);

        wasm_bindgen_futures::spawn_local(async move {
            // A panicking task must never unwind across the JavaScript
            // boundary (ADR 0020). The wasm32 target defaults to
            // `panic = "abort"`, which ends the worker outright; for builds
            // that opt into unwinding, this containment catches the panic,
            // reports it through tracing, and still signals completion so the
            // engine's cancel-then-join close path cannot hang on it.
            if std::panic::AssertUnwindSafe(Abortable::new(future, registration))
                .catch_unwind()
                .await
                .is_err()
            {
                tracing::error!("task panicked on the WASM runtime");
            }
            finished_for_task.set(true);
            let _ = completed_tx.send(());
        });

        TaskHandle {
            abort,
            completed: completed_rx,
            finished,
        }
    }

    async fn yield_now(&self) {
        // On the single-threaded worker, yielding means handing control back
        // to the microtask queue: a task spawned through `spawn_local` runs
        // only after this future suspends.
        let (tx, rx) = futures_channel::oneshot::channel();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = tx.send(());
        });
        let _ = rx.await;
    }

    async fn sleep(&self, duration: Duration) {
        #[cfg(target_family = "wasm")]
        {
            // gloo-timers accepts u32 but casts it to the signed setTimeout
            // argument internally, so every chunk must fit in i32.
            let mut duration = duration;
            let max_millis = i32::MAX as u32;
            let max_chunk = Duration::from_millis(max_millis as u64);
            while duration > max_chunk {
                gloo_timers::future::TimeoutFuture::new(max_millis).await;
                duration -= max_chunk;
            }
            let millis = duration.as_millis().max(1) as u32;
            gloo_timers::future::TimeoutFuture::new(millis).await;
        }

        // The public-API gate selects wasm32 cfg branches while rustdoc still
        // targets the host. That synthetic target cannot execute this private
        // runtime path, but it must type-check the public WASM surface.
        #[cfg(not(target_family = "wasm"))]
        let _ = duration;
    }
}

/// An abortable task that can be joined without detaching its work.
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]
pub(crate) struct TaskHandle(tokio::task::JoinHandle<()>);

#[cfg(target_arch = "wasm32")]
pub(crate) struct TaskHandle {
    abort: futures_util::future::AbortHandle,
    completed: futures_channel::oneshot::Receiver<()>,
    finished: Rc<Cell<bool>>,
}

#[cfg(any(
    all(not(target_arch = "wasm32"), feature = "runtime-tokio"),
    target_arch = "wasm32",
))]
impl TaskHandle {
    #[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]
    pub(crate) fn abort(&self) {
        self.0.abort();
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn abort(&self) {
        self.abort.abort();
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]
    pub(crate) fn is_finished(&self) -> bool {
        self.0.is_finished()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn is_finished(&self) -> bool {
        self.finished.get()
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]
    pub(crate) async fn join(self) {
        let _ = self.0.await;
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]
    pub(crate) async fn join_result(&mut self) -> Result<(), tokio::task::JoinError> {
        (&mut self.0).await
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn join(self) {
        let _ = self.completed.await;
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), feature = "runtime-tokio"))]
mod tests {
    use std::future::pending;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{Runtime, TokioRuntime};

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn aborting_a_task_then_joining_waits_for_its_future_to_drop() {
        let dropped = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&dropped);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let runtime = TokioRuntime;
        let handle = runtime.spawn(async move {
            let _signal = DropSignal(signal);
            let _ = started_tx.send(());
            pending::<()>().await;
        });

        started_rx.await.expect("task starts before it is aborted");
        handle.abort();
        handle.join().await;

        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn yield_now_hands_execution_back_to_the_runtime() {
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let handle = TokioRuntime.spawn(async move {
            TokioRuntime.yield_now().await;
            let _ = tx.send(());
        });

        handle.join().await;
        assert!(
            rx.try_recv().is_ok(),
            "the yielding task completes after yielding"
        );
    }
}
