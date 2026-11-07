//! compatible with std::sync::condvar except for both thread and coroutine
//! please ref the doc from std::sync::condvar
use crossbeam::queue::SegQueue;

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::{LockResult, PoisonError};
use std::time::Duration;

use super::blocking::SyncBlocker;
use super::mutex::{self, Mutex, MutexGuard};
use crate::cancel::{trigger_cancel_panic, Cancel};
use crate::park::ParkError;

/// restores cancel delivery on drop, so that a panic between
/// `disable_cancel` and `enable_cancel` cannot leave the coroutine with
/// cancel permanently disabled (the cancel data state is inflated by 2 per
/// disable and only deflated by a matching enable)
struct CancelDisableGuard(&'static Cancel);

impl CancelDisableGuard {
    fn new(cancel: &'static Cancel) -> Self {
        cancel.disable_cancel();
        CancelDisableGuard(cancel)
    }
}

impl Drop for CancelDisableGuard {
    fn drop(&mut self) {
        self.0.enable_cancel();
    }
}

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

/// A condition variable, usable from both threads and coroutines.
///
/// Waits take a [`Mutex`] guard; the guard is released while waiting and
/// re-acquired before the wait returns.
///
/// Unlike `std::sync::Condvar`, a `Condvar` is permanently bound to the first
/// mutex used with it: waiting with a different mutex panics even when no
/// waiter is currently queued.
pub struct Condvar {
    // the waiting blocker list
    // NOTE: a blocker that exits the wait via timeout/cancel without being
    // notified stays in the queue (with the release flag set) until a
    // notify pops it, so the signal can be forwarded to the next live
    // waiter. If a condvar is never notified again, those blockers are
    // retained for the lifetime of the condvar.
    to_wake: SegQueue<Arc<SyncBlocker>>,
    // used to verify the same mutex instance
    mutex: AtomicUsize,
}

impl Condvar {
    /// Create a new condition variable ready to be waited on and notified.
    pub const fn new() -> Condvar {
        Condvar {
            to_wake: SegQueue::new(),
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
        let _g = cancel.map(CancelDisableGuard::new);

        self.to_wake.push(cur.clone());

        // unlock the mutex to let other continue
        mutex::unlock_mutex(lock);
        // re-enable cancel before parking: a pending cancel is delivered
        // by the park itself
        drop(_g);

        // wait until coming back
        let ret = cur.park(dur);

        // when timed out or canceled, register the release action BEFORE
        // the re-lock below: the re-lock can block indefinitely, and a
        // notify that pops our blocker while we are blocked must forward
        // the signal to the next live waiter instead of delivering it to
        // a waiter that is already leaving
        if ret.is_err() {
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

        // disable cancel panic
        let _g = cancel.map(CancelDisableGuard::new);
        // don't run the guard destructor
        ::std::mem::forget(lock.lock());

        // drop the parker here without panic!
        // it may panic in the drop of Park
        drop(cur);

        // enable cancel panic (guard drop)
        drop(_g);

        ret
    }

    /// Block the current thread or coroutine until this condition variable is
    /// notified.
    ///
    /// The mutex guard is released while waiting and re-acquired before this
    /// function returns. Notification does not guarantee that the predicate a
    /// caller is interested in holds, so waiters should re-check it in a loop
    /// ([`wait_while`](Self::wait_while) does that for you).
    ///
    /// # Panics
    ///
    /// In a coroutine context, panics if the coroutine is canceled while
    /// waiting; the guard is released first. Panics if the guard belongs to a
    /// different mutex than the one this condition variable is bound to.
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

    /// Wait until `condition` returns `false`.
    ///
    /// Equivalent to calling [`wait`](Self::wait) in a loop while the
    /// predicate holds.
    ///
    /// # Panics
    ///
    /// Same as [`wait`](Self::wait).
    pub fn wait_while<'a, T, F>(
        &self,
        mut guard: MutexGuard<'a, T>,
        mut condition: F,
    ) -> LockResult<MutexGuard<'a, T>>
    where
        F: FnMut(&mut T) -> bool,
    {
        while condition(&mut *guard) {
            guard = self.wait(guard)?;
        }
        Ok(guard)
    }

    /// Wait until `condition` returns `false` or the timeout expires.
    ///
    /// Returns the re-acquired guard and a [`WaitTimeoutResult`] indicating
    /// whether the timeout was reached; in that case the predicate may still
    /// hold.
    ///
    /// # Panics
    ///
    /// Same as [`wait`](Self::wait).
    pub fn wait_timeout_while<'a, T, F>(
        &self,
        mut guard: MutexGuard<'a, T>,
        dur: Duration,
        mut condition: F,
    ) -> LockResult<(MutexGuard<'a, T>, WaitTimeoutResult)>
    where
        F: FnMut(&mut T) -> bool,
    {
        while condition(&mut *guard) {
            match self.wait_timeout(guard, dur) {
                Ok((g, timeout_result)) => {
                    if timeout_result.timed_out() {
                        return Ok((g, timeout_result));
                    }
                    guard = g;
                }
                Err(e) => return Err(e),
            }
        }
        Ok((guard, WaitTimeoutResult(false)))
    }

    /// Block until notified or `dur` elapses.
    ///
    /// The guard is released while waiting and re-acquired before this
    /// function returns. Returns a [`WaitTimeoutResult`] telling whether the
    /// timeout expired.
    ///
    /// Note: [`WaitTimeoutResult`] is may's own type; `std::sync::Condvar`
    /// returns `std::sync::WaitTimeoutResult`, whose constructor is private,
    /// so the two types are not interchangeable.
    ///
    /// # Panics
    ///
    /// Same as [`wait`](Self::wait).
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

    /// Wake one waiter, if any.
    ///
    /// Unlike a semaphore, the notification is not retained when nobody is
    /// waiting.
    pub fn notify_one(&self) {
        // loop instead of recursing on the release chain: each popped
        // blocker may have registered a release action (it timed out or was
        // canceled and won't take the lock), in which case the signal must
        // be passed on to the next waiter. A cancel storm can queue many
        // released blockers back to back, and a recursive call would
        // overflow the (small, 32KB default) coroutine stack and abort
        loop {
            let w = self.to_wake.pop();

            if let Some(w) = w {
                w.unpark();
                // when release is taken, pass the signal to the next waiter
                if !w.take_release() {
                    break;
                }
            } else {
                break;
            }
        }
    }

    /// Wake all waiters.
    pub fn notify_all(&self) {
        while let Some(w) = self.to_wake.pop() {
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

impl fmt::Debug for Condvar {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Condvar").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use crate::sleep::sleep;
    use crate::sync::mpsc::channel;
    use crate::sync::{Condvar, Mutex};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_const_new() {
        static C: Condvar = Condvar::new();
        C.notify_one();
        C.notify_all();
    }

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
                let (lock, cond) = &*data;
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

        let (lock, cond) = &*data;
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
                let (lock, cond) = &*data;
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

        let (lock, cond) = &*data;
        rx.recv().unwrap();
        let mut cnt = lock.lock().unwrap();
        assert_eq!(*cnt, N);
        *cnt = 0;
        drop(cnt);
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));

        const M: usize = 3;
        // we cancel some of the coroutine
        for co in vec.iter().take(M) {
            co.coroutine().cancel();
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

    #[test]
    fn test_wait_timeout_while() {
        let m = Arc::new(Mutex::new(false));
        let c = Arc::new(Condvar::new());

        // condition already false: returns immediately without timing out
        let g = m.lock().unwrap();
        let (g, to) = c
            .wait_timeout_while(g, Duration::from_millis(1), |p| *p)
            .unwrap();
        assert!(!to.timed_out());
        drop(g);

        // condition stays true: the wait times out
        {
            let mut g = m.lock().unwrap();
            *g = true;
        }
        let g = m.lock().unwrap();
        let (g, to) = c
            .wait_timeout_while(g, Duration::from_millis(1), |p| *p)
            .unwrap();
        assert!(to.timed_out());
        drop(g);

        // condition flipped by another thread: returns without timing out
        {
            let mut g = m.lock().unwrap();
            *g = false;
        }
        let m2 = m.clone();
        let c2 = c.clone();
        let g = m.lock().unwrap();
        let _t = thread::spawn(move || {
            let mut g = m2.lock().unwrap();
            *g = true;
            c2.notify_one();
            drop(g);
        });
        let (g, to) = c
            .wait_timeout_while(g, Duration::from_secs(2), |p| !*p)
            .unwrap();
        assert!(!to.timed_out());
        drop(g);
    }

    // regression: a waiter that timed out while the notifier holds the
    // mutex must not consume the signal meant for the live waiter behind
    // it in the queue — the release action is registered before the
    // blocking mutex re-lock
    #[test]
    fn test_timed_out_waiter_forwards_notify() {
        let m = Arc::new(Mutex::new(()));
        let m1 = m.clone();
        let m2 = m.clone();
        let m3 = m.clone();
        let c = Arc::new(Condvar::new());
        let c1 = c.clone();
        let c2 = c.clone();
        let c3 = c.clone();
        let (tx, rx) = channel();

        // W1: first in the condvar queue, times out quickly
        let h1 = go!(move || {
            let g = m1.lock().unwrap();
            let (g, to) = c1.wait_timeout(g, Duration::from_millis(100)).unwrap();
            assert!(to.timed_out());
            drop(g);
        });

        // W2: second in the queue, must receive the forwarded signal
        let h2 = go!(move || {
            sleep(Duration::from_millis(20)); // let W1 enqueue first
            let g = m2.lock().unwrap();
            tx.send(()).unwrap(); // parked in the condvar wait below
            let (g, to) = c2.wait_timeout(g, Duration::from_secs(2)).unwrap();
            assert!(!to.timed_out());
            drop(g);
        });

        // W2 holds the mutex, about to park: this lock blocks until W2's
        // wait unlocks the mutex. W1's 100ms timeout fires while we hold
        // it, so W1 blocks in its mutex re-lock.
        rx.recv().unwrap();
        let g = m3.lock().unwrap();
        sleep(Duration::from_millis(200));
        // pops W1's blocker: the signal must be forwarded to W2
        c3.notify_one();
        drop(g);
        h1.join().unwrap();
        h2.join().unwrap();
    }

    // regression: same as above but with a canceled waiter instead of a
    // timed out one
    #[test]
    fn test_canceled_waiter_forwards_notify() {
        let m = Arc::new(Mutex::new(()));
        let m1 = m.clone();
        let m2 = m.clone();
        let m3 = m.clone();
        let c = Arc::new(Condvar::new());
        let c1 = c.clone();
        let c2 = c.clone();
        let c3 = c.clone();
        let (tx, rx) = channel();

        // W1: first in the condvar queue, will be canceled
        let h1 = go!(move || {
            let g = m1.lock().unwrap();
            let _ = c1.wait(g).unwrap(); // cancel panic propagates
            unreachable!();
        });

        // W2: second in the queue, must receive the forwarded signal
        let h2 = go!(move || {
            sleep(Duration::from_millis(20)); // let W1 enqueue first
            let g = m2.lock().unwrap();
            tx.send(()).unwrap(); // parked in the condvar wait below
            let (g, to) = c2.wait_timeout(g, Duration::from_secs(2)).unwrap();
            assert!(!to.timed_out());
            drop(g);
        });

        // W2 holds the mutex, about to park: this lock blocks until W2's
        // wait unlocks the mutex. We cancel W1 while holding it: W1
        // registers its release action, then blocks in its mutex re-lock.
        rx.recv().unwrap();
        let g = m3.lock().unwrap();
        h1.coroutine().cancel();
        sleep(Duration::from_millis(100));
        // pops W1's blocker: the signal must be forwarded to W2
        c3.notify_one();
        drop(g);
        assert!(h1.join().is_err());
        h2.join().unwrap();
    }

    // regression: a chain of released blockers must be walked iteratively
    // — a recursive notify_one would overflow the coroutine stack
    #[test]
    fn test_notify_one_release_chain() {
        const N: usize = 500;

        let m = Arc::new(Mutex::new(()));
        let c = Arc::new(Condvar::new());
        let (tx_timeout, rx_timeout) = channel();
        let (tx_live, rx_live) = channel();
        let mut handles = vec![];
        for _ in 0..N - 1 {
            let m = m.clone();
            let c = c.clone();
            let tx = tx_timeout.clone();
            handles.push(go!(move || {
                let g = m.lock().unwrap();
                let (g, to) = c.wait_timeout(g, Duration::from_millis(10)).unwrap();
                assert!(to.timed_out());
                drop(g);
                tx.send(()).unwrap();
            }));
        }
        drop(tx_timeout);

        // live waiter: enqueues last, must receive the forwarded signal
        let m_live = m.clone();
        let c_live = c.clone();
        let h = go!(move || {
            sleep(Duration::from_millis(30)); // let the timeouts enqueue first
            let g = m_live.lock().unwrap();
            tx_live.send(()).unwrap(); // about to park below
            let (g, to) = c_live.wait_timeout(g, Duration::from_secs(2)).unwrap();
            assert!(!to.timed_out());
            drop(g);
        });

        // wait for all timed-out waiters to exit (release registered)
        for _ in 0..N - 1 {
            rx_timeout.recv().unwrap();
        }
        rx_live.recv().unwrap();
        // blocks until the live waiter's wait unlocks the mutex, so the
        // notify below finds its blocker in the queue
        let g = m.lock().unwrap();
        drop(g);
        // walks the whole release chain and wakes the live waiter
        c.notify_one();

        for h in handles {
            h.join().unwrap();
        }
        h.join().unwrap();
    }
}
