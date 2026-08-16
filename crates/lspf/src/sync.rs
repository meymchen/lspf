//! Portable synchronization primitives for the protocol kernel (ADR 0020).
//!
//! The kernel must compile for `wasm32-unknown-unknown` without enabling a
//! Tokio runtime, so the concurrency limit of ADR 0012 lives on a
//! target-agnostic counting semaphore here instead of a tokio type.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

/// A target-agnostic counting semaphore: `permits` concurrent acquisitions
/// are allowed before further acquisitions wait for a release.
///
/// Releases happen when an owned permit drops, so a permit that outlives its
/// acquisition future keeps its slot occupied. A release wakes the
/// longest-waiting acquirer and hands it one slot; a waiter whose future was
/// cancelled before it polled again never holds the handoff hostage — the
/// handoff stays claimable by the next live acquirer. The semaphore is never
/// closed.
pub(crate) struct Semaphore {
    inner: Mutex<Inner>,
}

struct Inner {
    /// Slots sitting free in the semaphore.
    permits: usize,
    /// Slots already handed to a woken waiter but not yet taken by a poll.
    /// Keeping them separate from `permits` is what makes a released slot
    /// survive a stale waker: a cancelled waiter that never polls again
    /// cannot lose the slot it was woken for.
    waking: usize,
    waiters: VecDeque<Waker>,
}

impl Semaphore {
    /// A semaphore with `permits` initial slots, ready to share across
    /// clones of the returned `Arc`.
    pub(crate) fn shared(permits: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                permits,
                waking: 0,
                waiters: VecDeque::new(),
            }),
        })
    }

    /// Acquire one slot, waiting until some holder releases it.
    pub(crate) fn acquire_owned(self: Arc<Self>) -> Acquire {
        Acquire { sem: self }
    }
}

/// The future for [`Semaphore::acquire_owned`].
pub(crate) struct Acquire {
    sem: Arc<Semaphore>,
}

impl Future for Acquire {
    type Output = OwnedPermit;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut inner = self.sem.inner.lock().unwrap();
        if inner.permits > 0 {
            inner.permits -= 1;
            return Poll::Ready(OwnedPermit {
                sem: Arc::clone(&self.sem),
            });
        }
        // A slot may already be in handoff from a release that woke a waiter
        // which has since been cancelled: this poller claims it instead, so
        // no slot is ever lost to a stale waker.
        if inner.waking > 0 {
            inner.waking -= 1;
            return Poll::Ready(OwnedPermit {
                sem: Arc::clone(&self.sem),
            });
        }
        // Register this task's waker once per wait. Wakers wake spuriously on
        // contention, so every poll re-checks the slot count above.
        if !inner
            .waiters
            .back()
            .is_some_and(|waker| waker.will_wake(cx.waker()))
        {
            inner.waiters.push_back(cx.waker().clone());
        }
        Poll::Pending
    }
}

/// One acquired slot, released back to its [`Semaphore`] on drop.
pub(crate) struct OwnedPermit {
    sem: Arc<Semaphore>,
}

impl Drop for OwnedPermit {
    fn drop(&mut self) {
        let mut inner = self.sem.inner.lock().unwrap();
        // Hand the freed slot to the longest-waiting acquirer. The slot moves
        // into `waking` until a poll claims it, so a waiter that was woken but
        // never polls again (cancelled future) cannot make the slot vanish:
        // the next live acquirer takes the handoff. A woken waiter may lose
        // the race to another acquirer and re-queue itself; that is the
        // standard condition-variable wake semantics.
        if let Some(waker) = inner.waiters.pop_front() {
            inner.waking += 1;
            waker.wake();
        } else {
            inner.permits += 1;
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    use tokio::time::{Duration, timeout};

    use super::Semaphore;

    /// A waker that records whether `wake` ever fired, so manual polling can
    /// observe wake-up delivery without an executor.
    #[derive(Default)]
    struct FlagWaker(Arc<AtomicBool>);

    impl Wake for FlagWaker {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl FlagWaker {
        fn new() -> (Self, Arc<AtomicBool>) {
            let flag = Arc::new(AtomicBool::new(false));
            (Self(Arc::clone(&flag)), flag)
        }

        fn poll_once<F: Future>(&self, future: &mut Pin<Box<F>>) -> Poll<F::Output> {
            let waker = Waker::from(Arc::new(FlagWaker(Arc::clone(&self.0))));
            let mut cx = Context::from_waker(&waker);
            future.as_mut().poll(&mut cx)
        }
    }

    #[tokio::test]
    async fn a_dropped_permit_frees_its_slot() {
        let semaphore = Semaphore::shared(1);
        let permit = semaphore.clone().acquire_owned().await;
        drop(permit);

        timeout(Duration::from_secs(5), semaphore.clone().acquire_owned())
            .await
            .expect("the freed slot is immediately re-acquirable");
    }

    #[tokio::test]
    async fn acquisition_waits_while_the_limit_is_exhausted() {
        let semaphore = Semaphore::shared(1);
        let held = semaphore.clone().acquire_owned().await;

        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
        let contender = semaphore.clone();
        tokio::spawn(async move {
            let _permit = contender.acquire_owned().await;
            let _ = done_tx.send(());
        });

        assert!(
            timeout(Duration::from_millis(50), &mut done_rx)
                .await
                .is_err(),
            "no slot is free while the one permit is held"
        );

        drop(held);
        let _ = timeout(Duration::from_secs(5), done_rx)
            .await
            .expect("releasing the held permit unblocks the waiter");
    }

    #[test]
    fn a_release_wakes_only_the_longest_waiting_acquirer() {
        let semaphore = Semaphore::shared(1);
        let poller = FlagWaker::default();

        let mut held = Box::pin(semaphore.clone().acquire_owned());
        let held_permit = match poller.poll_once(&mut held) {
            Poll::Ready(permit) => permit,
            Poll::Pending => panic!("the first acquisition takes the one free slot"),
        };

        // Three contenders register in deterministic order, each with its own
        // observable waker.
        let mut waiters: Vec<(Pin<Box<_>>, Arc<AtomicBool>)> = Vec::new();
        for _ in 0..3 {
            let (waker, flag) = FlagWaker::new();
            let mut acquire = Box::pin(semaphore.clone().acquire_owned());
            assert!(waker.poll_once(&mut acquire).is_pending());
            assert!(!flag.load(Ordering::SeqCst), "no slot frees yet");
            waiters.push((acquire, flag));
        }

        drop(held_permit);

        assert!(
            waiters[0].1.load(Ordering::SeqCst),
            "the longest-waiting acquirer is woken first"
        );
        assert!(
            !waiters[1].1.load(Ordering::SeqCst) && !waiters[2].1.load(Ordering::SeqCst),
            "one release wakes exactly one waiter"
        );

        // The woken waiter takes the slot; a second release wakes the next.
        let (mut first, _) = waiters.remove(0);
        let permit = match poller.poll_once(&mut first) {
            Poll::Ready(permit) => permit,
            Poll::Pending => panic!("the woken waiter acquires the freed slot"),
        };
        drop(permit);
        assert!(waiters[0].1.load(Ordering::SeqCst));
        assert!(!waiters[1].1.load(Ordering::SeqCst));
    }

    #[test]
    fn a_cancelled_waiter_cannot_lose_a_released_slot() {
        let semaphore = Semaphore::shared(1);
        let poller = FlagWaker::default();

        let mut held = Box::pin(semaphore.clone().acquire_owned());
        let held_permit = match poller.poll_once(&mut held) {
            Poll::Ready(permit) => permit,
            Poll::Pending => panic!("the first acquisition takes the one free slot"),
        };

        // A registers and is then cancelled without ever polling again: its
        // waker stays in the queue, but no poll will ever claim a handoff.
        let (waker_a, flag_a) = FlagWaker::new();
        let mut a = Box::pin(semaphore.clone().acquire_owned());
        assert!(waker_a.poll_once(&mut a).is_pending());
        drop(a);

        // B registers afterwards.
        let (waker_b, flag_b) = FlagWaker::new();
        let mut b = Box::pin(semaphore.clone().acquire_owned());
        assert!(waker_b.poll_once(&mut b).is_pending());
        assert!(!flag_b.load(Ordering::SeqCst), "no slot frees yet");

        drop(held_permit);

        // The release wakes the stale A waker — harmlessly — and parks the
        // slot in handoff; B's next poll claims it instead of losing it.
        assert!(
            flag_a.load(Ordering::SeqCst),
            "the release wakes the longest-waiting waker, stale or not"
        );
        assert!(!flag_b.load(Ordering::SeqCst));
        match poller.poll_once(&mut b) {
            Poll::Ready(_permit) => {}
            Poll::Pending => panic!("the released slot must reach the live waiter"),
        }
    }
}
