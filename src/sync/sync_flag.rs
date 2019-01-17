use std::fmt;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::blocking::SyncBlocker;
use cancel::trigger_cancel_panic;
use crossbeam::sync::SegQueue;
use park::ParkError;

/// SyncFlag primitive
///
/// SyncFlag allow threads and coroutines to synchronize their actions
/// like barrier.
///
/// A SyncFlag is an boolean value
/// When the SyncFlag is false, any thread or coroutine wait on it would
/// block until it's value becomes true
/// When the SyncFalg is true, any thread or coroutine wait on it would
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
    // track how many resources available for the SyncFlag
    // if it's negative means how many threads are waiting for
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

    #[inline]
    fn wakeup_all(&self) {
        while let Some(w) = self.to_wake.try_pop() {
            w.unpark();
            if w.take_release() {
                self.fire();
            }
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
            Ok(_) => true,
            Err(err) => {
                // check the unpark status
                if cur.is_unparked() {
                    self.fire();
                } else {
                    // register
                    cur.set_release();
                    // re-check unpark status
                    if cur.is_unparked() && cur.take_release() {
                        self.fire();
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

    /// wait for a SyncFlag
    /// if the SyncFlag value is bigger than zero the function returns immediately
    /// otherwise it would block the until a `fire` is executed
    pub fn wait(&self) {
        self.wait_timeout_impl(None);
    }

    /// same as `wait` except that with an extra timeout value
    /// return false if timeout happened
    pub fn wait_timeout(&self, dur: Duration) -> bool {
        self.wait_timeout_impl(Some(dur))
    }

    /// set the SyncFlag to true
    /// and would wakeup all threads/coroutines that are calling `wait`
    pub fn fire(&self) {
        self.cnt.store(std::isize::MAX, Ordering::SeqCst);

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
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

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
        use sleep::sleep;

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
        println!("flag1={:?}", flag1);
        // cancel h1
        unsafe { h1.coroutine().cancel() };
        h1.join().unwrap_err();
        // release the SyncFlag
        flag1.fire();
        h2.join().unwrap();
    }

    #[test]
    fn test_syncflag_co_timeout() {
        use sleep::sleep;

        let flag1 = Arc::new(SyncFlag::new());
        let flag2 = flag1.clone();
        let flag3 = flag1.clone();

        let h1 = go!(move || {
            let r = flag2.wait_timeout(Duration::from_millis(10));
            assert_eq!(r, false);
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
        use sleep::sleep;

        let flag1 = Arc::new(SyncFlag::new());
        let flag2 = flag1.clone();
        let flag3 = flag1.clone();

        let h1 = thread::spawn(move || {
            let r = flag2.wait_timeout(Duration::from_millis(10));
            assert_eq!(r, false);
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
}
