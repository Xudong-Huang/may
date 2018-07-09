use std::fmt;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::blocking::SyncBlocker;
use cancel::trigger_cancel_panic;
use crossbeam::sync::SegQueue;
use park::ParkError;

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

impl Semphore {
    /// create a semphore with the initial value
    pub fn new(init: usize) -> Self {
        assert!(init < ::std::isize::MAX as usize);
        Semphore {
            to_wake: SegQueue::new(),
            cnt: AtomicIsize::new(init as isize),
        }
    }

    #[inline]
    fn wakeup_one(&self) {
        self.to_wake.try_pop().map_or_else(
            || panic!("got null blocker!"),
            |w| {
                w.unpark();
                if w.take_release() {
                    self.post();
                }
            },
        );
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

    /// wait for a semphore
    /// if the semphore value is bigger than zero the function returns immediately
    /// otherwise it would block the until a `post` is executed
    pub fn wait(&self) {
        self.wait_timeout_impl(None);
    }

    /// same as `wait` except that with an extra timeout value
    /// return false if timeout happened
    pub fn wait_timeout(&self, dur: Duration) -> bool {
        self.wait_timeout_impl(Some(dur))
    }

    /// return false if would block
    /// return true if successfully acquire one semphore resource
    pub fn try_wait(&self) -> bool {
        // we not register ourself at all
        // just manipulate the cnt is enough
        let mut cnt = self.cnt.load(Ordering::Acquire);
        while cnt > 0 {
            match self
                .cnt
                .compare_exchange(cnt, cnt - 1, Ordering::SeqCst, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(x) => cnt = x,
            }
        }
        false
    }

    /// increment the semphore value
    /// and would wakeup a thread/coroutine that is calling `wait`
    pub fn post(&self) {
        let cnt = self.cnt.fetch_add(1, Ordering::SeqCst);
        assert!(cnt < ::std::isize::MAX);

        // try to wakeup one waiter first
        if cnt < 0 {
            self.wakeup_one();
        }
    }

    /// return the current semphore value
    pub fn get_value(&self) -> usize {
        let cnt = self.cnt.load(Ordering::Acquire);
        if cnt > 0 {
            return cnt as usize;
        }
        0
    }
}

impl fmt::Debug for Semphore {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let cnt = self.cnt.load(Ordering::Acquire);
        write!(f, "Semphore {{ cnt: {} }}", cnt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;
    use sync::mpsc::channel;

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
        println!("sem={:?}", sem);
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
        println!("sem={:?}", sem);

        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(sum, (0..total).sum());
    }

    #[test]
    fn test_semphore_canceled() {
        use sleep::sleep;

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
        println!("sem1={:?}", sem1);
        // cancel h1
        unsafe { h1.coroutine().cancel() };
        h1.join().unwrap_err();
        // release the semphore
        sem1.post();
        h2.join().unwrap();
    }

    #[test]
    fn test_semphore_co_timeout() {
        use sleep::sleep;

        let sem1 = Arc::new(Semphore::new(0));
        let sem2 = sem1.clone();
        let sem3 = sem1.clone();

        let h1 = go!(move || {
            let r = sem2.wait_timeout(Duration::from_millis(10));
            assert_eq!(r, false);
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
        use sleep::sleep;

        let sem1 = Arc::new(Semphore::new(0));
        let sem2 = sem1.clone();
        let sem3 = sem1.clone();

        let h1 = thread::spawn(move || {
            let r = sem2.wait_timeout(Duration::from_millis(10));
            assert_eq!(r, false);
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
}
