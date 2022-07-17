//! compatible with std::sync::condvar except for both thread and coroutine
//! please ref the doc from std::sync::condvar
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::{LockResult, PoisonError};
use std::time::Duration;

use crate::cancel::trigger_cancel_panic;
use crate::park::ParkError;
use may_queue::spsc;

use super::blocking::SyncBlocker;
use super::mutex::{self, Mutex, MutexGuard};

/// A type indicating whether a timed wait on a condition variable returned
/// due to a time out or not.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    /// Returns whether the wait was known to have timed out.
    pub fn timed_out(&self) -> bool {
        self.0
    }
}

pub struct Condvar {
    // the waiting blocker list
    to_wake: Mutex<spsc::Queue<Arc<SyncBlocker>>>,
    // used to verify the same mutex instance
    mutex: AtomicUsize,
}

impl Condvar {
    pub fn new() -> Condvar {
        Condvar {
            to_wake: Mutex::new(spsc::Queue::new()),
            mutex: AtomicUsize::new(0),
        }
    }

    // return false if timeout happened
    pub fn wait_impl<T>(&self, lock: &Mutex<T>, dur: Option<Duration>) -> Result<(), ParkError> {
        let cancel = if crate::coroutine_impl::is_coroutine() {
            Some(crate::coroutine_impl::current_cancel_data())
        } else {
            None
        };
        // enqueue the blocker
        let cur = SyncBlocker::current();

        // we can't cancel panic here!!
        if let Some(c) = cancel.as_ref() {
            c.disable_cancel();
        }

        let g = self.to_wake.lock().unwrap();
        g.push(cur.clone());
        drop(g);

        // unlock the mutex to let other continue
        mutex::unlock_mutex(lock);
        if let Some(c) = cancel.as_ref() {
            c.enable_cancel();
        }

        // wait until coming back
        let ret = cur.park(dur);
        // disable cancel panic
        if let Some(c) = cancel.as_ref() {
            c.disable_cancel();
        }
        // don't run the guard destructor
        ::std::mem::forget(lock.lock());

        if ret.is_err() {
            // when in a cancel state, could cause problem for the lock
            // make notify never panic by disable the cancel bit

            // check the unpark status
            if cur.is_unparked() {
                self.notify_one();
            } else {
                // register
                cur.set_release();
                // re-check unpark status
                if cur.is_unparked() && cur.take_release() {
                    self.notify_one();
                }
            }
        }

        // drop the parker here without panic!
        // it may panic in the drop of Park
        drop(cur);

        // enable cancel panic
        if let Some(c) = cancel.as_ref() {
            c.enable_cancel();
        }

        ret
    }

    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> LockResult<MutexGuard<'a, T>> {
        let poisoned = {
            let lock = mutex::guard_lock(&guard);
            self.verify(lock as *const _ as usize);
            let ret = self.wait_impl(lock, None);
            if ret == Err(ParkError::Canceled) {
                // don't set the poison flag
                ::std::mem::forget(guard);
                // release the mutex to let other run
                mutex::unlock_mutex(lock);
                // now we can safely go with the cancel panic
                trigger_cancel_panic();
            }
            mutex::guard_poison(&guard).get()
        };
        if poisoned {
            Err(PoisonError::new(guard))
        } else {
            Ok(guard)
        }
    }

    pub fn wait_timeout<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        dur: Duration,
    ) -> LockResult<(MutexGuard<'a, T>, WaitTimeoutResult)> {
        let (poisoned, result) = {
            let lock = mutex::guard_lock(&guard);
            self.verify(lock as *const _ as usize);
            let ret = self.wait_impl(lock, Some(dur));
            if ret == Err(ParkError::Canceled) {
                // don't set the poison flag
                ::std::mem::forget(guard);
                // release the mutex to let other run
                mutex::unlock_mutex(lock);
                // now we can safely go with the cancel panic
                trigger_cancel_panic();
            }
            (
                mutex::guard_poison(&guard).get(),
                WaitTimeoutResult(ret.is_err()),
            )
        };
        if poisoned {
            Err(PoisonError::new((guard, result)))
        } else {
            Ok((guard, result))
        }
    }

    pub fn notify_one(&self) {
        // NOTICE: the following code would not drop the lock!
        // if let Some(w) = self.to_wake.lock().unwrap().pop() {

        let g = self.to_wake.lock().unwrap();
        let w = g.pop();
        drop(g);

        if let Some(w) = w {
            w.unpark();
            if w.take_release() {
                self.notify_one();
            }
        }
    }

    pub fn notify_all(&self) {
        let g = self.to_wake.lock().unwrap();
        while let Some(w) = g.pop() {
            w.unpark();
        }
    }

    fn verify(&self, addr: usize) {
        match self
            .mutex
            .compare_exchange(0, addr, Ordering::SeqCst, Ordering::SeqCst)
        {
            // If we got out 0, then we have successfully bound the mutex to
            // this condvar.
            Ok(0) => {}

            // If we get out a value that's the same as `addr`, then someone
            // already beat us to the punch.
            Err(n) if n == addr => {}

            // Anything else and we're using more than one mutex on this condvar,
            // which is currently disallowed.
            _ => panic!("attempted to use a condition variable with two mutex"),
        }
    }
}

impl Default for Condvar {
    fn default() -> Condvar {
        Condvar::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::sync::mpsc::channel;
    use crate::sync::{Condvar, Mutex};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;
    use std::u32;

    #[test]
    fn smoke() {
        let c = Condvar::new();
        c.notify_one();
        c.notify_all();
    }

    #[test]
    fn notify_one() {
        let m = Arc::new(Mutex::new(()));
        let m2 = m.clone();
        let c = Arc::new(Condvar::new());
        let c2 = c.clone();

        let g = m.lock().unwrap();
        let _t = thread::spawn(move || {
            let _g = m2.lock().unwrap();
            c2.notify_one();
        });
        let g = c.wait(g).unwrap();
        drop(g);
    }

    #[test]
    fn notify_all() {
        const N: usize = 10;

        let data = Arc::new((Mutex::new(0), Condvar::new()));
        let (tx, rx) = channel();
        for _ in 0..N {
            let data = data.clone();
            let tx = tx.clone();
            thread::spawn(move || {
                let &(ref lock, ref cond) = &*data;
                let mut cnt = lock.lock().unwrap();
                *cnt += 1;
                if *cnt == N {
                    tx.send(()).unwrap();
                }
                while *cnt != 0 {
                    cnt = cond.wait(cnt).unwrap();
                }
                tx.send(()).unwrap();
            });
        }
        drop(tx);

        let &(ref lock, ref cond) = &*data;
        rx.recv().unwrap();
        let mut cnt = lock.lock().unwrap();
        assert_eq!(*cnt, N);
        *cnt = 0;
        cond.notify_all();
        drop(cnt);
        for _ in 0..N {
            rx.recv().unwrap();
        }
    }

    #[test]
    fn wait_timeout() {
        let m = Arc::new(Mutex::new(()));
        let m2 = m.clone();
        let c = Arc::new(Condvar::new());
        let c2 = c.clone();

        let g = m.lock().unwrap();
        let (g, _no_timeout) = c.wait_timeout(g, Duration::from_millis(1)).unwrap();
        // spurious wakeup mean this isn't necessarily true
        // assert!(!no_timeout);
        let _t = thread::spawn(move || {
            let _g = m2.lock().unwrap();
            c2.notify_one();
        });
        let (g, timeout_res) = c
            .wait_timeout(g, Duration::from_millis(u32::MAX as u64))
            .unwrap();
        assert!(!timeout_res.timed_out());
        drop(g);
    }

    #[test]
    #[should_panic]
    fn two_mutex() {
        let m = Arc::new(Mutex::new(()));
        let m2 = m.clone();
        let c = Arc::new(Condvar::new());
        let c2 = c.clone();

        let mut g = m.lock().unwrap();
        let _t = thread::spawn(move || {
            let _g = m2.lock().unwrap();
            c2.notify_one();
        });
        g = c.wait(g).unwrap();
        drop(g);

        let m = Mutex::new(());
        let _ = c.wait(m.lock().unwrap()).unwrap();
    }

    #[test]
    fn test_condvar_canceled() {
        use std::sync::mpsc::TryRecvError;
        const N: usize = 10;

        let data = Arc::new((Mutex::new(0), Condvar::new()));
        let (tx, rx) = channel();
        let mut vec = vec![];
        for _ in 0..N {
            let data = data.clone();
            let tx = tx.clone();
            let h = go!(move || {
                let &(ref lock, ref cond) = &*data;
                let mut cnt = lock.lock().unwrap();
                *cnt += 1;
                if *cnt == N {
                    tx.send(()).unwrap();
                }
                while *cnt != 0 {
                    cnt = cond.wait(cnt).unwrap();
                }
                tx.send(()).unwrap();
            });
            vec.push(h);
        }
        drop(tx);

        let &(ref lock, ref cond) = &*data;
        rx.recv().unwrap();
        let mut cnt = lock.lock().unwrap();
        assert_eq!(*cnt, N);
        *cnt = 0;
        drop(cnt);
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));

        const M: usize = 3;
        // we cancel some of the coroutine
        for co in vec.iter().take(M) {
            unsafe { co.coroutine().cancel() };
        }

        // note that cancel and notify has contention here
        // which could cause some coroutine can't get any signal
        // if the cancelled coroutine unparked successfully
        // make sure every coroutine get the correct signal
        // crate::coroutine::sleep(::std::time::Duration::from_millis(1));
        // or use `notify_all()` to make sure all the coroutines
        // get a signal

        cond.notify_all();
        for _ in 0..N - M {
            // cond.notify_one();
            rx.recv().unwrap();
        }

        for h in vec {
            h.join().ok();
        }

        // for most cases this assertion is correct
        // rarely try_recv would return Ok(())
        // assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
    }
}
