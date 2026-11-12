use std::fmt;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::blocking::{cancel_pending, SyncBlocker};
use crate::cancel::trigger_cancel_panic;
use crate::park::ParkError;
use crossbeam::queue::SegQueue;

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
/// // spawn a coroutine, and then wait for it to start
/// unsafe {
///     coroutine::spawn(move || {
///         sem2.post();
///     });
/// }
///
/// // wait for the coroutine to start up
/// sem.wait();
/// ```
pub struct Semphore {
    // track how many resources available for the semphore
    // if it's negative means how many threads are waiting for
    cnt: AtomicIsize,
    // the waiting blocker list, must be mpmc
    to_wake: SegQueue<Arc<SyncBlocker>>,
}

/// Alias for [`Semphore`] using the conventional spelling.
pub type Semaphore = Semphore;

/// Result of [`Semphore::wait_timeout_until`].
pub(crate) enum WaitStatus {
    /// A permit was acquired (or the wait was notified).
    Acquired,
    /// The `stop` predicate was true after the waiter registered; no permit
    /// was consumed.
    Stopped,
    /// The timeout expired.
    Timeout,
}

impl Semphore {
    /// create a semphore with the initial value
    pub fn new(init: usize) -> Self {
        assert!(init < isize::MAX as usize);
        Semphore {
            to_wake: SegQueue::new(),
            cnt: AtomicIsize::new(init as isize),
        }
    }

    // wake up the first live waiter, forwarding the permit past waiters that
    // have already timed out or been canceled (their `release` flag is set).
    //
    // Returns `true` if a live waiter took the permit and `false` if there
    // was no live waiter queued, in which case the permit stays in `cnt`.
    //
    // The walk must not stop on a positive `cnt`: waiters that already
    // registered (and are queued behind a released blocker) are covered by
    // permits, so `cnt >= 0` does not mean that no live waiter is left.
    //
    // Iterative on purpose: a cancel storm can leave many released blockers
    // back to back in the queue, and recursing through `post` would overflow
    // the (small, 32KB default) coroutine stack and abort.
    #[inline]
    fn wakeup_one(&self) -> bool {
        while let Some(w) = self.to_wake.pop() {
            // `true` means a live waiter took the permit
            if w.try_hand_off() {
                return true;
            }

            // the released waiter no longer counts as waiting (`cnt += 1`);
            // keep looking for a live waiter to forward the permit to
            self.cnt.fetch_add(1, Ordering::SeqCst);
        }

        // The queue is empty although the counter said otherwise. Receivers
        // that pushed a blocker but have not yet decremented `cnt`, or that
        // were already woken by a broadcast drain, are the common cause; the
        // permit simply stays in `cnt`.
        false
    }

    /// Account an already-pushed blocker as a waiter.
    ///
    /// A permit can be posted between the fast path and this registration.
    /// Hand it to the oldest waiter; if there is none, it stays available in
    /// `cnt`.
    #[inline]
    fn record_waiter(&self) {
        // dec the cnt, if it's positive, unpark one waiter
        if self.cnt.fetch_sub(1, Ordering::SeqCst) > 0 {
            self.wakeup_one();
        }
    }

    /// Wake every queued waiter.
    ///
    /// This is a broadcast for a predicate that has become permanently true
    /// (an MPMC receiver observing a disconnect): each queued blocker is
    /// retired (`cnt += 1` undoes its registration) and its waiter is
    /// unparked. Receivers that pushed a blocker but have not yet run
    /// `record_waiter` are woken too; the same `+1` retires the registration
    /// they are about to add.
    ///
    /// Unlike [`post`](Self::post), the wakeup is not permit based, so
    /// waiters that were not yet accounted for cannot make it stop early.
    pub(crate) fn wake_all(&self) {
        while let Some(w) = self.to_wake.pop() {
            // retire the waiter's registration, exactly like the
            // released-blocker path in `wakeup_one`
            self.cnt.fetch_add(1, Ordering::SeqCst);
            // live waiters are unparked; released blockers are just dropped
            w.try_hand_off();
        }
    }

    // return false if timeout
    fn wait_timeout_impl(&self, dur: Option<Duration>) -> bool {
        matches!(self.wait_timeout_until(dur, || false), WaitStatus::Acquired)
    }

    /// Wait for a permit, but leave without parking if `stop()` becomes true
    /// after the waiter has been registered.
    ///
    /// Used by modules whose wakeup source can be "disconnected" (MPMC): the
    /// registration's `cnt` RMW pairs with the producer's `post`, so a
    /// disconnect that happened before the registration is observed by the
    /// `stop()` check, and one that happens afterwards wakes the waiter.
    pub(crate) fn wait_timeout_until<F>(&self, dur: Option<Duration>, stop: F) -> WaitStatus
    where
        F: Fn() -> bool,
    {
        // try wait first
        if self.try_wait() {
            return WaitStatus::Acquired;
        }

        let cur = SyncBlocker::current();
        // register blocker first
        self.to_wake.push(cur.clone());
        // dec the cnt, and forward a permit that was posted in between to the
        // oldest waiter (which can end up being this one)
        self.record_waiter();

        // Re-check after registering; the fetch_sub above synchronizes with
        // the producer's post through the SeqCst modification order of `cnt`.
        if stop() {
            // A post may have handed us a permit concurrently. If so, consume
            // it: a queued item must be delivered before the caller reports a
            // disconnect. Only leave without the permit if none was acquired.
            if cur.leave_wait() {
                // As in the parked case, a cancel can lose to that post and
                // only set the bit; forward the permit rather than deliver it
                // to a canceled waiter.
                if cancel_pending() {
                    self.post();
                    trigger_cancel_panic();
                }
                return WaitStatus::Acquired;
            }
            return WaitStatus::Stopped;
        }

        match cur.park(dur) {
            Ok(_) => {
                // A post can lose the race with `cancel`: the permit is handed
                // over by `try_hand_off`, so the cancel finds no registration
                // left and only sets the bit. Forward the permit to the next
                // waiter and unwind instead of returning to a canceled waiter.
                if cancel_pending() {
                    if cur.leave_wait() {
                        self.post();
                    }
                    trigger_cancel_panic();
                }
                WaitStatus::Acquired
            }
            Err(err) => {
                // a permit may have been handed to us concurrently; we are
                // leaving, so it has to be forwarded to the next waiter
                if cur.leave_wait() {
                    self.post();
                }

                // now we can safely go with the cancel panic
                if err == ParkError::Canceled {
                    trigger_cancel_panic();
                }
                WaitStatus::Timeout
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
    /// later `post` pops it and forwards the permit, so a timeout-polling
    /// loop retains one blocker per timeout until the next post.
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
        let prev = loop {
            let cur = self.cnt.load(Ordering::SeqCst);
            assert!(cur < isize::MAX, "semaphore permit count overflow");
            match self
                .cnt
                .compare_exchange_weak(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break cur,
                Err(_) => continue,
            }
        };

        // try to wakeup one waiter first
        if prev < 0 {
            self.wakeup_one();
        }
    }

    /// Return the current semaphore value.
    ///
    /// Queued waiters are not reported: the value is clamped to `0` while
    /// the internal counter is negative.
    pub fn get_value(&self) -> usize {
        let cnt = self.cnt.load(Ordering::SeqCst);
        if cnt > 0 {
            return cnt as usize;
        }
        0
    }
}

impl fmt::Debug for Semphore {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let cnt = self.cnt.load(Ordering::SeqCst);
        f.debug_struct("Semphore").field("cnt", &cnt).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::mpsc::channel;
    use std::thread;

    #[test]
    fn sanity_1() {
        let sem = Arc::new(Semphore::new(0));
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
        let sem = Arc::new(Semphore::new(init));
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

        let sem1 = Arc::new(Semphore::new(0));
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

        let sem1 = Arc::new(Semphore::new(0));
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

        let sem1 = Arc::new(Semphore::new(0));
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
        let sem = Semphore::new(2);
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

    /// Regression test for the release-forwarding chain: canceled waiters
    /// leave released blockers queued, and a single `post` must walk past all
    /// of them. The old recursive implementation overflowed the 32KB
    /// coroutine stack on a chain this long.
    #[test]
    fn released_waiters_are_forwarded_iteratively() {
        use crate::sleep::sleep;

        const N: usize = 1000;

        let sem = Arc::new(Semphore::new(0));
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

        // run the post (and thus the forwarding chain) on a coroutine stack
        let s = sem.clone();
        go!(move || s.post()).join().unwrap();

        assert_eq!(sem.get_value(), 1);
        assert!(sem.try_wait());
        assert_eq!(sem.get_value(), 0);
    }

    /// Regression test for the MPMC disconnect wakeup.
    ///
    /// A waiter pushes its blocker before it decrements `cnt`. Two waiters
    /// preempted inside that window used to absorb the disconnect posts and
    /// the spare permit, so `drop_tx`'s post-while-waiters loop stopped while
    /// a third, fully registered waiter was still parked. A broadcast drain
    /// must wake it regardless of the counter.
    ///
    /// Completing the registration of the two window waiters also exercises
    /// `wakeup_one` on an empty queue: their `fetch_sub` sees the permits
    /// retired by the drain and must not panic with "got null blocker!".
    #[test]
    fn broadcast_wakeup_ignores_unaccounted_waiters() {
        use std::time::Instant;

        let sem = Arc::new(Semphore::new(0));

        // two waiters are preempted right after pushing their blocker and
        // before decrementing the counter
        let w1 = SyncBlocker::current();
        let w2 = SyncBlocker::current();
        sem.to_wake.push(w1.clone());
        sem.to_wake.push(w2.clone());

        // a third waiter registers completely and parks
        let waiter = {
            let sem = sem.clone();
            thread::spawn(move || sem.wait_timeout(Duration::from_secs(5)))
        };
        let start = Instant::now();
        while sem.cnt.load(Ordering::SeqCst) != -1 {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "waiter did not register"
            );
            thread::yield_now();
        }

        // the producers broadcast the disconnect
        sem.wake_all();

        // the window waiters now finish their registration
        for w in [&w1, &w2] {
            sem.record_waiter();
            assert!(w.is_unparked());
        }

        assert!(waiter.join().unwrap(), "parked waiter was not woken");
        assert_eq!(sem.cnt.load(Ordering::SeqCst), 0);
    }
}
