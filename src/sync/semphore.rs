use std::fmt;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::blocking::{cancel_pending, SyncBlocker};
use crate::cancel::trigger_cancel_panic;
use crate::park::ParkError;
use crossbeam::queue::SegQueue;

/// Alias for [`Semaphore`] using the conventional spelling.
pub type Semphore = Semaphore;

/// Semphore primitive
///
/// semaphores allow threads and coroutines to synchronize their actions.
///
/// A semaphore is an integer whose value is never allowed to fall below
/// zero.  Two operations can be performed on semaphores: increment the
/// semaphore value by one (post()); and decrement the semaphore
/// value by one (wait()).  If the value of a semaphore is currently
/// zero, then a wait() operation will block until the value becomes
/// greater than zero.
///
/// A wake-up is advisory: it does not carry a permit. A woken waiter
/// re-checks the counter and blocks again if another waiter consumed the
/// permit first. This keeps the counter authoritative even when a `post`,
/// `try_wait` and a waiter leaving (timeout/cancel) race, so a permit can
/// never be handed out twice.
///
/// # Examples
///
/// ```rust
/// use std::sync::Arc;
/// use may::coroutine;
/// use may::sync::Semphore;
///
/// let sem = Arc::new(Semphore::new(0));
/// let sem2 = sem.clone();
///
/// unsafe {
///     coroutine::spawn(move || {
///         sem2.post();
///     });
/// }
///
/// // wait for the coroutine to start up
/// sem.wait();
/// ```
pub struct Semaphore {
    // how many permits are available; only `try_wait` decrements it
    cnt: AtomicIsize,
    // the waiting blocker list, must be mpmc
    to_wake: SegQueue<Arc<SyncBlocker>>,
}

/// Result of [`Semphore::wait_timeout_until`].
pub(crate) enum WaitStatus {
    /// A permit was acquired.
    Acquired,
    /// The `stop` predicate was true; no permit was consumed.
    Stopped,
    /// The timeout expired.
    Timeout,
}

impl Semaphore {
    /// create a semphore with the initial value
    pub fn new(init: usize) -> Self {
        assert!(init < isize::MAX as usize);
        Semaphore {
            to_wake: SegQueue::new(),
            cnt: AtomicIsize::new(init as isize),
        }
    }

    /// Wake the first live waiter.
    ///
    /// The wake-up is advisory: the woken waiter re-checks the permit count,
    /// so it is harmless for the wake to race with [`try_wait`](Self::try_wait)
    /// or with a waiter that timed out. A waiter that has already left
    /// (timeout or cancel) only leaves a released blocker behind; it is
    /// skipped so it cannot swallow the wake-up meant for a waiter behind it.
    ///
    /// Iterative on purpose: a cancel storm can leave many released blockers
    /// back to back in the queue, and recursing through `post` would overflow
    /// the (small) default coroutine stack and abort.
    #[inline]
    fn wakeup_one(&self) {
        while let Some(w) = self.to_wake.pop() {
            if w.try_hand_off() {
                return;
            }
        }
    }

    /// Wake every queued waiter.
    ///
    /// Used for a predicate that has become permanently true (an MPMC
    /// receiver observing a disconnect): every queued blocker is unparked,
    /// and released blockers are just dropped.
    pub(crate) fn wake_all(&self) {
        while let Some(w) = self.to_wake.pop() {
            w.try_hand_off();
        }
    }

    // return false if timeout
    fn wait_timeout_impl(&self, dur: Option<Duration>) -> bool {
        matches!(self.wait_timeout_until(dur, || false), WaitStatus::Acquired)
    }

    /// Wait for a permit, but leave without blocking if `stop()` becomes true
    /// once the waiter has been published to the wake-up list.
    ///
    /// Used by modules whose wakeup source can be "disconnected" (MPMC): the
    /// waiter is queued before the predicate is evaluated, so either this
    /// check observes the disconnect or the disconnect's `wake_all` observes
    /// the waiter.
    pub(crate) fn wait_timeout_until<F>(&self, dur: Option<Duration>, stop: F) -> WaitStatus
    where
        F: Fn() -> bool,
    {
        // a duration too large to be represented as an instant is treated as
        // an infinite wait instead of overflowing
        let deadline = dur.and_then(|d| Instant::now().checked_add(d));

        loop {
            // try wait first
            if self.try_wait() {
                return WaitStatus::Acquired;
            }
            if stop() {
                return WaitStatus::Stopped;
            }

            let cur = SyncBlocker::current();
            // register the blocker before re-checking the counter: a post that
            // races with the registration either observes the blocker and
            // wakes it, or has already published its permit and the re-check
            // below consumes it
            self.to_wake.push(cur.clone());
            if self.try_wait() {
                // mark the blocker released so a later wake-up skips it
                // instead of handing the wake-up to a waiter that is gone
                cur.leave_wait();
                return WaitStatus::Acquired;
            }

            // Re-check the stop predicate *after* publishing the blocker: a
            // disconnect that happened before the push is observed here, and
            // one that happens afterwards wakes the blocker via `wake_all`.
            if stop() {
                cur.leave_wait();
                return WaitStatus::Stopped;
            }

            let remaining = match deadline {
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        Some(Duration::ZERO)
                    } else {
                        Some(d - now)
                    }
                }
                None => None,
            };

            match cur.park(remaining) {
                Ok(_) => {
                    // A wake-up can lose the race with `cancel`: the wake was
                    // consumed for this waiter, so pass it on to the next one
                    // before unwinding.
                    if cancel_pending() {
                        if cur.leave_wait() {
                            self.wakeup_one();
                        }
                        trigger_cancel_panic();
                    }
                    // advisory wake-up: re-check the counter. If another
                    // waiter consumed the permit first, register again and
                    // keep waiting with the remaining time.
                }
                Err(err) => {
                    // A wake-up may have raced with the timeout. Wakes are
                    // advisory, so there is no permit to return, but the
                    // woken state must be passed on to the next waiter.
                    if cur.leave_wait() {
                        self.wakeup_one();
                    }

                    // now we can safely go with the cancel panic
                    if err == ParkError::Canceled {
                        trigger_cancel_panic();
                    }
                    return WaitStatus::Timeout;
                }
            }
        }
    }

    /// Wait for a semaphore.
    ///
    /// If the semaphore value is bigger than zero the function returns
    /// immediately, otherwise it blocks until a `post` is executed.
    ///
    /// # Panics
    ///
    /// In a coroutine context, panics if the coroutine is canceled while
    /// waiting.
    pub fn wait(&self) {
        self.wait_timeout_impl(None);
    }

    /// Same as [`wait`](Self::wait), but returns `false` if the timeout
    /// expired before a permit became available.
    ///
    /// A timed-out waiter leaves its blocker in the internal queue until a
    /// later wakeup drains it, so a timeout-polling loop retains one blocker
    /// per timeout until the next `post`.
    ///
    /// # Panics
    ///
    /// In a coroutine context, panics if the coroutine is canceled while
    /// waiting.
    #[must_use = "returns false when the timeout expired"]
    pub fn wait_timeout(&self, dur: Duration) -> bool {
        self.wait_timeout_impl(Some(dur))
    }

    /// Try to acquire one semaphore resource without blocking.
    ///
    /// Returns `true` if a permit was acquired and `false` if the call would
    /// block.
    #[must_use = "returns false if the semaphore would block"]
    pub fn try_wait(&self) -> bool {
        // we not register ourself at all
        // just manipulate the cnt is enough
        let mut cnt = self.cnt.load(Ordering::SeqCst);
        while cnt > 0 {
            match self
                .cnt
                .compare_exchange(cnt, cnt - 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return true,
                Err(x) => cnt = x,
            }
        }
        false
    }

    /// Increment the semaphore value and wake up a thread/coroutine that is
    /// calling [`wait`](Self::wait), if any.
    ///
    /// # Panics
    ///
    /// Panics if the permit count would overflow `isize::MAX`.
    pub fn post(&self) {
        // CAS loop so the counter is not wrapped before the overflow check
        // can fire
        loop {
            let cur = self.cnt.load(Ordering::SeqCst);
            assert!(cur < isize::MAX, "semaphore permit count overflow");
            match self
                .cnt
                .compare_exchange_weak(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break,
                Err(_) => continue,
            }
        }

        // the wake-up is advisory, so a permit that was stolen by a racing
        // `try_wait` cannot be handed out twice; the woken waiter re-checks
        // the counter instead
        self.wakeup_one();
    }

    /// Return the current semaphore value.
    pub fn get_value(&self) -> usize {
        let cnt = self.cnt.load(Ordering::SeqCst);
        if cnt > 0 {
            return cnt as usize;
        }
        0
    }
}

impl fmt::Debug for Semaphore {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let cnt = self.cnt.load(Ordering::SeqCst);
        f.debug_struct("Semaphore").field("cnt", &cnt).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::mpsc::channel;
    use std::thread;

    #[test]
    fn sanity_1() {
        let sem = Arc::new(Semaphore::new(0));
        let sem2 = sem.clone();

        // spawn a new thread, and then wait for it to start
        thread::spawn(move || {
            sem2.post();
        });

        // wait for the thread to start up
        sem.wait();
    }

    #[test]
    fn sanity_2() {
        let total = 10;
        let init = 5;
        let sem = Arc::new(Semaphore::new(init));
        let (tx, rx) = channel();

        // create 10 thread and let them wait for the semphore
        println!("sem={sem:?}");
        for i in 0..total {
            let sem2 = sem.clone();
            let tx2 = tx.clone();
            go!(move || {
                sem2.wait();
                tx2.send(i).unwrap();
            });
        }

        let mut sum = 0;
        for _i in 0..init {
            sum += rx.recv().unwrap();
        }

        // thread::sleep(Duration::from_secs(1));
        // println!("sem={:?}", sem);

        use std::sync::mpsc::TryRecvError;
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));

        for _i in 0..total - init {
            sem.post();
        }

        for _i in 0..total - init {
            sum += rx.recv().unwrap();
        }
        println!("sem={sem:?}");

        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(sum, (0..total).sum());
    }

    #[test]
    fn test_semphore_canceled() {
        use crate::sleep::sleep;

        let sem1 = Arc::new(Semaphore::new(0));
        let sem2 = sem1.clone();
        let sem3 = sem1.clone();

        let h1 = go!(move || {
            sem2.wait();
        });

        let h2 = go!(move || {
            // let h1 enqueue
            sleep(Duration::from_millis(50));
            sem3.wait();
        });

        // wait h1 and h2 enqueue
        sleep(Duration::from_millis(100));
        println!("sem1={sem1:?}");
        // cancel h1
        unsafe { h1.coroutine().cancel() };
        h1.join().unwrap_err();
        // release the semphore
        sem1.post();
        h2.join().unwrap();
    }

    #[test]
    fn test_semphore_co_timeout() {
        use crate::sleep::sleep;

        let sem1 = Arc::new(Semaphore::new(0));
        let sem2 = sem1.clone();
        let sem3 = sem1.clone();

        let h1 = go!(move || {
            let r = sem2.wait_timeout(Duration::from_millis(10));
            assert!(!r);
        });

        let h2 = go!(move || {
            // let h1 enqueue
            sleep(Duration::from_millis(50));
            sem3.wait();
        });

        // wait h1 timeout
        h1.join().unwrap();
        // release the semphore
        sem1.post();
        h2.join().unwrap();
    }

    #[test]
    fn test_semphore_thread_timeout() {
        use crate::sleep::sleep;

        let sem1 = Arc::new(Semaphore::new(0));
        let sem2 = sem1.clone();
        let sem3 = sem1.clone();

        let h1 = thread::spawn(move || {
            let r = sem2.wait_timeout(Duration::from_millis(10));
            assert!(!r);
        });

        let h2 = thread::spawn(move || {
            // let h1 enqueue
            sleep(Duration::from_millis(50));
            sem3.wait();
        });

        // wait h1 timeout
        h1.join().unwrap();
        // release the semphore
        sem1.post();
        h2.join().unwrap();
    }

    #[test]
    fn try_wait_and_get_value() {
        let sem = Semaphore::new(2);
        assert_eq!(sem.get_value(), 2);
        assert!(sem.try_wait());
        assert_eq!(sem.get_value(), 1);
        assert!(sem.try_wait());
        assert!(!sem.try_wait());
        assert_eq!(sem.get_value(), 0);

        sem.post();
        assert_eq!(sem.get_value(), 1);
        assert!(sem.try_wait());
        assert_eq!(sem.get_value(), 0);
    }

    /// Regression test for the wake-up chain: canceled waiters leave released
    /// blockers queued, and a single `post` must walk past all of them. The
    /// old recursive implementation overflowed the coroutine stack (then
    /// 32KB by default) on a chain this long.
    #[test]
    fn released_waiters_are_skipped_iteratively() {
        use crate::sleep::sleep;

        const N: usize = 1000;

        let sem = Arc::new(Semaphore::new(0));
        let waiters: Vec<_> = (0..N)
            .map(|_| {
                let s = sem.clone();
                go!(move || s.wait())
            })
            .collect();

        // let the waiters enqueue, then cancel them all; each leaves a
        // released blocker behind
        sleep(Duration::from_millis(50));
        for w in &waiters {
            unsafe { w.coroutine().cancel() };
        }
        for w in waiters {
            assert!(w.join().is_err());
        }

        // run the post (and thus the wake-up walk) on a coroutine stack
        let s = sem.clone();
        go!(move || s.post()).join().unwrap();

        assert_eq!(sem.get_value(), 1);
        assert!(sem.try_wait());
        assert_eq!(sem.get_value(), 0);
    }

    /// A wake-up must walk past released blockers: one that timed out must not
    /// swallow the wake-up meant for a live waiter behind it.
    #[test]
    fn broadcast_wakeup_skips_released_blockers() {
        let sem = Arc::new(Semaphore::new(0));

        let stale = SyncBlocker::current();
        sem.to_wake.push(stale.clone());
        assert!(!stale.leave_wait());

        let live = SyncBlocker::current();
        sem.to_wake.push(live.clone());

        sem.wake_all();

        assert!(!stale.is_unparked());
        assert!(live.is_unparked());
        assert_eq!(sem.to_wake.len(), 0);
    }

    /// A wake-up must not deliver a permit by itself. If a fast-path
    /// `try_wait` consumes the permit that triggered the wake-up, the woken
    /// waiter has to keep waiting instead of reporting an acquisition.
    ///
    /// The old protocol let a `post` hand its permit to a waiter while the
    /// permit stayed in the counter, so a racing `try_wait` could consume it
    /// too; the woken waiter then popped an empty queue (the mpmc
    /// "a semaphore permit had no queued item" failure).
    #[test]
    fn advisory_wakeup_does_not_deliver_a_permit() {
        use crate::sleep::sleep;

        let sem = Arc::new(Semaphore::new(0));
        let s = sem.clone();
        let waiter = go!(move || s.wait_timeout(Duration::from_millis(300)));

        // let the waiter register and park
        sleep(Duration::from_millis(50));
        sem.post();

        if !sem.try_wait() {
            // the woken waiter won the race and consumed the permit; the
            // interleaving of interest did not happen
            let _ = waiter.join();
            return;
        }

        // the permit was consumed by the fast path, so the woken waiter must
        // report a timeout rather than a phantom acquisition
        assert!(
            !waiter.join().unwrap(),
            "woken waiter reported a permit that was already consumed"
        );
    }

    /// A permit posted while a waiter is registering must reach it: the
    /// waiter is published before the counter is re-checked, so one of the
    /// two sides sees the other.
    #[test]
    fn post_racing_with_registration_is_not_lost() {
        for _ in 0..200 {
            let sem = Arc::new(Semaphore::new(0));
            let s = sem.clone();
            let waiter = thread::spawn(move || s.wait_timeout(Duration::from_secs(5)));

            // race the post with the registration
            sem.post();
            assert!(waiter.join().unwrap(), "wake-up was lost");
        }

        // and the reverse order: post before the waiter registers
        for _ in 0..200 {
            let sem = Arc::new(Semaphore::new(0));
            sem.post();
            let s = sem.clone();
            let waiter = thread::spawn(move || s.wait_timeout(Duration::from_secs(5)));
            assert!(waiter.join().unwrap(), "buffered permit was lost");
        }
    }
}
