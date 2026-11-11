use std::fmt;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::blocking::{cancel_pending, SyncBlocker};
use crate::cancel::trigger_cancel_panic;
use crate::park::ParkError;
use crossbeam::queue::SegQueue;

/// SyncFlag primitive
///
/// SyncFlag allow threads and coroutines to synchronize their actions
/// like barrier.
///
/// A SyncFlag is an boolean value
/// When the SyncFlag is false, any thread or coroutine wait on it would
/// block until it's value becomes true
/// When the SyncFlag is true, any thread or coroutine wait on it would
/// return immediately.
///
/// After the SyncFlag becomes true, it will never becomes false again.
///
/// # Examples
///
/// ```rust
/// use std::sync::Arc;
/// use may::coroutine;
/// use may::sync::SyncFlag;
///
/// let flag = Arc::new(SyncFlag::new());
/// let flag2 = flag.clone();
///
/// // spawn a coroutine, and then wait for it to start
/// unsafe {
///     coroutine::spawn(move || {
///         flag2.fire();
///         flag2.wait();
///     });
/// }
///
/// // wait for the coroutine to start up
/// flag.wait();
/// ```
pub struct SyncFlag {
    // waiter count: negative while waiters are queued, and `isize::MAX` once
    // fired (the "true" state)
    cnt: AtomicIsize,
    // the waiting blocker list, must be mpmc
    to_wake: SegQueue<Arc<SyncBlocker>>,
}

impl Default for SyncFlag {
    fn default() -> Self {
        SyncFlag {
            to_wake: SegQueue::new(),
            cnt: AtomicIsize::new(0),
        }
    }
}

impl SyncFlag {
    /// create a SyncFlag with the initial value
    pub fn new() -> Self {
        Default::default()
    }

    // Wake every waiter. `fire` is level-triggered (`cnt` is set to
    // `isize::MAX` and never drops back), so unlike a notify-one hand-off
    // there is no permit to forward past released blockers: just drain the
    // queue. Iterative because a cancel storm can leave many released
    // blockers queued, and re-entering `fire` recursively would overflow the
    // (small, 32KB default) coroutine stack.
    #[inline]
    fn wakeup_all(&self) {
        while let Some(w) = self.to_wake.pop() {
            // nothing needs to be forwarded for a broadcast wakeup; the
            // result only says whether the waiter was still live
            w.try_hand_off();
        }
    }

    // return false if timeout
    fn wait_timeout_impl(&self, dur: Option<Duration>) -> bool {
        // try wait first
        if self.is_fired() {
            return true;
        }

        let cur = SyncBlocker::current();
        // register blocker first
        self.to_wake.push(cur.clone());
        // dec the cnt, if it's positive, unpark one waiter
        if self.cnt.fetch_sub(1, Ordering::SeqCst) > 0 {
            self.wakeup_all();
        }

        match cur.park(dur) {
            Ok(_) => {
                // A `fire` can beat `cancel` and consume the park
                // registration, leaving only the cancel bit set. `fire` is
                // level-triggered, so no wakeup has to be forwarded, but the
                // wait must still unwind instead of returning to a canceled
                // coroutine.
                if cancel_pending() {
                    trigger_cancel_panic();
                }
                true
            }
            Err(err) => {
                // a fire may have woken us concurrently; a fire is
                // level-triggered so re-firing is harmless, but it must not be
                // lost for the remaining waiters
                if cur.leave_wait() {
                    self.fire();
                }

                // now we can safely go with the cancel panic
                if err == ParkError::Canceled {
                    trigger_cancel_panic();
                }
                false
            }
        }
    }

    /// Wait for a `SyncFlag`.
    ///
    /// If the flag is already fired the function returns immediately,
    /// otherwise it blocks until a `fire` is executed.
    ///
    /// # Panics
    ///
    /// In a coroutine context, panics if the coroutine is canceled while
    /// waiting.
    pub fn wait(&self) {
        self.wait_timeout_impl(None);
    }

    /// Same as [`wait`](Self::wait), but returns `false` if the timeout
    /// expired before the flag was fired.
    ///
    /// A timed-out waiter leaves its blocker in the internal queue until a
    /// later `fire` pops it, so a timeout-polling loop retains one blocker
    /// per timeout until the flag is fired.
    ///
    /// # Panics
    ///
    /// In a coroutine context, panics if the coroutine is canceled while
    /// waiting.
    #[must_use = "returns false when the timeout expired"]
    pub fn wait_timeout(&self, dur: Duration) -> bool {
        self.wait_timeout_impl(Some(dur))
    }

    /// set the SyncFlag to true
    /// and would wakeup all threads/coroutines that are calling `wait`
    pub fn fire(&self) {
        self.cnt.store(isize::MAX, Ordering::SeqCst);

        // try to wakeup all waiters
        self.wakeup_all();
    }

    /// return the current SyncFlag value
    pub fn is_fired(&self) -> bool {
        let cnt = self.cnt.load(Ordering::SeqCst);
        cnt > 0
    }
}

impl fmt::Debug for SyncFlag {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "SyncFlag {{ is_fired: {} }}", self.is_fired())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn sanity_test() {
        let flag = Arc::new(SyncFlag::new());
        let flag2 = flag.clone();

        // spawn a new thread, and then wait for it to start
        thread::spawn(move || {
            flag2.fire();
            flag2.wait();
        });

        // wait for the thread to start up
        flag.wait();
    }

    #[test]
    fn test_syncflag_canceled() {
        use crate::sleep::sleep;

        let flag1 = Arc::new(SyncFlag::new());
        let flag2 = flag1.clone();
        let flag3 = flag1.clone();

        let h1 = go!(move || {
            flag2.wait();
        });

        let h2 = go!(move || {
            // let h1 enqueue
            sleep(Duration::from_millis(50));
            flag3.wait();
        });

        // wait h1 and h2 enqueue
        sleep(Duration::from_millis(100));
        println!("flag1={flag1:?}");
        // cancel h1
        h1.coroutine().cancel();
        h1.join().unwrap_err();
        // release the SyncFlag
        flag1.fire();
        h2.join().unwrap();
    }

    #[test]
    fn test_syncflag_co_timeout() {
        use crate::sleep::sleep;

        let flag1 = Arc::new(SyncFlag::new());
        let flag2 = flag1.clone();
        let flag3 = flag1.clone();

        let h1 = go!(move || {
            let r = flag2.wait_timeout(Duration::from_millis(10));
            assert!(!r);
        });

        let h2 = go!(move || {
            // let h1 enqueue
            sleep(Duration::from_millis(50));
            flag3.wait();
        });

        // wait h1 timeout
        h1.join().unwrap();
        // release the SyncFlag
        flag1.fire();
        h2.join().unwrap();
    }

    #[test]
    fn test_syncflag_thread_timeout() {
        use crate::sleep::sleep;

        let flag1 = Arc::new(SyncFlag::new());
        let flag2 = flag1.clone();
        let flag3 = flag1.clone();

        let h1 = thread::spawn(move || {
            let r = flag2.wait_timeout(Duration::from_millis(10));
            assert!(!r);
        });

        let h2 = thread::spawn(move || {
            // let h1 enqueue
            sleep(Duration::from_millis(50));
            flag3.wait();
        });

        // wait h1 timeout
        h1.join().unwrap();
        // release the SyncFlag
        flag1.fire();
        h2.join().unwrap();
    }

    /// Regression test for the release-forwarding chain: canceled waiters
    /// leave released blockers queued, and a single `fire` must walk past all
    /// of them. The old implementation re-entered `fire` recursively and
    /// overflowed the 32KB coroutine stack on a chain this long.
    #[test]
    fn released_waiters_are_woken_iteratively() {
        use crate::sleep::sleep;

        const N: usize = 1000;

        let flag = Arc::new(SyncFlag::new());
        let waiters: Vec<_> = (0..N)
            .map(|_| {
                let f = flag.clone();
                go!(move || f.wait())
            })
            .collect();

        // let the waiters enqueue, then cancel them all; each leaves a
        // released blocker behind
        sleep(Duration::from_millis(50));
        for w in &waiters {
            w.coroutine().cancel();
        }
        for w in waiters {
            assert!(w.join().is_err());
        }

        // run the fire (and thus the drain) on a coroutine stack
        let f = flag.clone();
        go!(move || f.fire()).join().unwrap();

        assert!(flag.is_fired());
        // the fired state wakes future waiters immediately
        flag.wait();
    }
}
