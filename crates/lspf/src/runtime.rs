use std::future::Future;

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
/// Native tasks can move between Tokio worker threads, whereas a future WASM
/// runtime will keep them on the worker thread that spawned them.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) trait TaskSend: sealed::Sealed + Send {}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> TaskSend for T {}

#[cfg(target_arch = "wasm32")]
pub(crate) trait TaskSend: sealed::Sealed {}

#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> TaskSend for T {}

/// Crate-private task execution boundary.
pub(crate) trait Runtime {
    fn spawn<F>(&self, future: F) -> TaskHandle
    where
        F: Future<Output = ()> + TaskSend + 'static;
}

/// Runtime selected for native targets.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
pub(crate) struct TokioRuntime;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn default_runtime() -> TokioRuntime {
    TokioRuntime
}

#[cfg(not(target_arch = "wasm32"))]
impl Runtime for TokioRuntime {
    fn spawn<F>(&self, future: F) -> TaskHandle
    where
        F: Future<Output = ()> + TaskSend + 'static,
    {
        TaskHandle(tokio::spawn(future))
    }
}

/// Runtime selected for browser WASM targets.
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
        use futures_util::future::{AbortHandle, Abortable};

        let (abort, registration) = AbortHandle::new_pair();
        let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
        let finished = Rc::new(Cell::new(false));
        let finished_for_task = Rc::clone(&finished);

        wasm_bindgen_futures::spawn_local(async move {
            let _ = Abortable::new(future, registration).await;
            finished_for_task.set(true);
            let _ = completed_tx.send(());
        });

        TaskHandle {
            abort,
            completed: completed_rx,
            finished,
        }
    }
}

/// An abortable task that can be joined without detaching its work.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct TaskHandle(tokio::task::JoinHandle<()>);

#[cfg(target_arch = "wasm32")]
pub(crate) struct TaskHandle {
    abort: futures_util::future::AbortHandle,
    completed: tokio::sync::oneshot::Receiver<()>,
    finished: Rc<Cell<bool>>,
}

impl TaskHandle {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn abort(&self) {
        self.0.abort();
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn abort(&self) {
        self.abort.abort();
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn is_finished(&self) -> bool {
        self.0.is_finished()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn is_finished(&self) -> bool {
        self.finished.get()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn join(self) {
        let _ = self.0.await;
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn join(self) {
        let _ = self.completed.await;
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
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
}
