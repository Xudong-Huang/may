use std::fmt;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::blocking::SyncBlocker;
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
    // Iterative on purpose: a cancel storm can leave many released blockers
    // back to back in the queue, and recursing through `post` would overflow
    // the (small, 32KB default) coroutine stack and abort.
    #[inline]
    fn wakeup_one(&self) {
        loop {
            let w = self.to_wake.pop().expect("got null blocker!");

            w.unpark();
            if !w.take_release() {
                // a live waiter consumed the permit
                break;
            }

            // the released waiter no longer counts as waiting (`cnt += 1`);
            // forward the permit if there are still waiters behind it
            if self.cnt.fetch_add(1, Ordering::SeqCst) >= 0 {
                break;
            }
        }
    }

    // return false if timeout
    fn wait_timeout_impl(&self, dur: Option<Duration>) -> bool {
        // try wait first
        if self.try_wait() {
            return true;
        }

        let cur = SyncBlocker::current();
        // register blocker first
        self.to_wake.push(cur.clone());
        // dec the cnt, if it's positive, unpark one waiter
        if self.cnt.fetch_sub(1, Ordering::SeqCst) > 0 {
            self.wakeup_one();
        }

        match cur.park(dur) {
            Ok(_) => true,
            Err(err) => {
                // check the unpark status
                if cur.is_unparked() {
                    self.post();
                } else {
                    // register
                    cur.set_release();
                    // re-check unpark status
                    if cur.is_unparked() && cur.take_release() {
                        self.post();
                    }
                }

                // now we can safely go with the cancel panic
                if err == ParkError::Canceled {
                    trigger_cancel_panic();
                }
                false
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
        h1.coroutine().cancel();
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
            w.coroutine().cancel();
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
}
