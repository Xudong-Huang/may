use std::fmt;
use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::cancel::Cancel;
use crate::coroutine_impl::{co_cancel_data, run_coroutine, CoroutineImpl, EventSource};
use crate::scheduler::get_scheduler;
use crate::sync::atomic_dur::AtomicDuration;
use crate::sync::delay_drop::{DelayDrop, DropGuard};
use crate::sync::{AtomicOption, WaitCo};
use crate::timeout_list::TimeoutHandle;
use crate::yield_now::{get_co_para, yield_with};

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ParkError {
    Canceled,
    Timeout,
}

pub struct Park {
    // the coroutine that is waiting for this park instance. A fresh handle is
    // published for every wait, so a timer or cancel registration left over
    // from an earlier wait can only look into that earlier (already emptied)
    // handle: it can never take a later waiter and report a false timeout.
    wait_co: AtomicOption<WaitCo>,
    // when true means the Park no need to block
    state: AtomicBool,
    // control how to deal with the cancellation, usually init one time
    check_cancel: AtomicBool,
    // timeout settings in ms, 0 is none (park forever)
    timeout: AtomicDuration,
    // timer handle, can be null
    timeout_handle: AtomicOption<TimeoutHandle<WaitCo>>,
    // a flag if kernel is entered
    can_drop: DelayDrop,
}

impl Default for Park {
    fn default() -> Self {
        Park::new()
    }
}

// this is the park resource type (spmc style)
impl Park {
    pub fn new() -> Self {
        Park {
            wait_co: AtomicOption::none(),
            state: AtomicBool::new(false),
            check_cancel: AtomicBool::new(true),
            timeout: AtomicDuration::new(None),
            timeout_handle: AtomicOption::none(),
            can_drop: DelayDrop::new(),
        }
    }

    // ignore cancel, if true, caller have to do the check instead
    pub fn ignore_cancel(&self, ignore: bool) {
        self.check_cancel.store(!ignore, Ordering::Relaxed);
    }

    #[inline]
    fn set_timeout_handle(
        &self,
        handle: Option<TimeoutHandle<WaitCo>>,
    ) -> Option<TimeoutHandle<WaitCo>> {
        match handle {
            Some(h) => self.timeout_handle.swap(h),
            None => self.timeout_handle.take(),
        }
    }

    // return true if need park the coroutine
    // when the state is true, we clear it and indicate not to block
    // when the state is false, means we need real park
    #[inline]
    fn check_park(&self) -> bool {
        // fast check, since only one consumer to park
        if self.state.load(Ordering::Acquire) {
            self.state.store(false, Ordering::Release);
            return false;
        }
        !self.state.swap(false, Ordering::AcqRel)
    }

    // unpark the underlying coroutine if any
    #[inline]
    pub(crate) fn unpark_impl(&self, b_sync: bool) {
        if !self.state.swap(true, Ordering::AcqRel) {
            self.wake_up(b_sync);
        }
    }

    // unpark the underlying coroutine if any, push to the ready task queue
    #[inline]
    pub fn unpark(&self) {
        self.unpark_impl(false);
    }

    // remove the timeout handle after return back to user space
    #[inline]
    fn remove_timeout_handle(&self) {
        if let Some(h) = self.set_timeout_handle(None) {
            if h.is_link() {
                get_scheduler().del_timer(h);
            }
            // when timeout the node is unlinked
            // just drop it to release memory
        }
    }

    #[inline]
    fn wake_up(&self, b_sync: bool) {
        // take this wait's handle, then the coroutine from it: either a
        // concurrent timer/cancel took the coroutine first (handle empty),
        // or the take here wins and it is resumed exactly once
        if let Some(wait_co) = self.wait_co.take() {
            if let Some(co) = wait_co.take() {
                if b_sync {
                    run_coroutine(co);
                } else {
                    get_scheduler().schedule(co);
                }
            }
        }
    }

    /// park current coroutine with specified timeout
    /// if timeout happens, return Err(ParkError::Timeout)
    /// if cancellation detected, return Err(ParkError::Canceled)
    pub fn park_timeout(&self, dur: Option<Duration>) -> Result<(), ParkError> {
        // if the state is not set, need to wait
        if !self.check_park() {
            return Ok(());
        }

        // before a new yield wait the kernel done
        self.can_drop.wait();

        self.timeout.store(dur);

        // what if the state is set before yield?
        // the subscribe would re-check it
        yield_with(self);
        // clear the trigger state
        self.check_park();
        // remove timer handle
        self.remove_timeout_handle();

        // let _gen = self.state.load(Ordering::Acquire);
        // println!("unparked gen={}, self={:p}", gen, self);

        if let Some(err) = get_co_para() {
            match err.kind() {
                ErrorKind::TimedOut => return Err(ParkError::Timeout),
                ErrorKind::Other => return Err(ParkError::Canceled),
                _ => unreachable!("unexpected return error kind"),
            }
        }

        Ok(())
    }

    fn delay_drop(&self) -> DropGuard<'_> {
        self.can_drop.delay_drop()
    }
}

impl Drop for Park {
    fn drop(&mut self) {
        // wait the kernel finish
        self.can_drop.wait();

        self.set_timeout_handle(None);
    }
}

impl EventSource for Park {
    // register the coroutine to the park
    fn subscribe(&mut self, co: CoroutineImpl) {
        let cancel = co_cancel_data(&co);
        // this wait owns the timeout; dropping it means the wait is over
        let timeout = self.timeout.take();

        let _g = self.delay_drop();

        // One hand-off handle per wait. The timer and the cancel machinery
        // hold their own clones, so a timer that outlived its wait (its
        // removal only reaches the timer thread asynchronously) can only
        // look into that wait's already emptied handle, never into a later
        // waiter's.
        let wait_co = Arc::new(AtomicOption::some(co));

        // Register for cancellation before publishing the waiter: once it is
        // published another worker may resume the coroutine and start the
        // next wait, so the shared `Cancel` slot must not be written after
        // that.
        cancel.set_co(wait_co.clone());
        if cancel.is_canceled() {
            // the coroutine was taken and scheduled by `cancel`; there is no
            // wait left that needs a timer
            cancel.cancel();
            return;
        }

        // register the coroutine
        self.wait_co.store(wait_co.clone());

        // re-check the state, only clear once after resume
        if self.state.load(Ordering::Acquire) {
            // Do not resume the coroutine inline here. The resumed coroutine
            // can drop the last reference to the owner of this Park (e.g. a
            // cancelled Mutex/RwLock waiter dropping its SyncBlocker) while
            // this `subscribe` is still using it. `DelayDrop::wait` would wait
            // for that subscribe to finish, but it yields the coroutine first,
            // and a cancelled coroutine cannot yield (yield_now short-circuits
            // on the cancel bit), so an inline resume deadlocks the worker.
            // Hand the wake to the scheduler instead: this worker's queue
            // usually picks it up right after `subscribe` returns, so the
            // latency is the same, but the coroutine can no longer run while
            // `subscribe` is still on the stack.
            return self.wake_up(false);
        }

        // Arm the timer last. A short timeout can fire at once; by now the
        // timer already owns the coroutine through `wait_co`, so firing
        // immediately wakes this waiter instead of being dropped on an empty
        // slot and stranding the coroutine until some unrelated unpark.
        let timeout_handle = timeout.map(|dur| get_scheduler().add_timer(dur, wait_co));
        self.set_timeout_handle(timeout_handle);
    }

    // when the cancel is true we check the panic or do nothing
    fn yield_back(&self, cancel: &'static Cancel) {
        if self.check_cancel.load(Ordering::Relaxed) {
            cancel.check_cancel();
        }
    }
}

impl fmt::Debug for Park {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Park").field("state", &self.state).finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::coroutine;

    /// A timer left over from an earlier `park_timeout` must not wake a later
    /// wait on the same coroutine.
    ///
    /// `remove_timeout_handle` only queues the removal for the timer thread,
    /// and the timer thread may already have popped the entry. If the timer
    /// data were shared between the waits of one `Park`, that stale timer
    /// would take whatever coroutine is registered next and report a false
    /// timeout.
    ///
    /// The test makes the window deterministic by occupying the timer thread:
    /// a coroutine resumed by the timeout handler runs on the timer thread, so
    /// a plain thread sleep there keeps the timer thread from firing other
    /// timers or processing its remove queue.
    #[test]
    fn stale_timer_does_not_wake_the_next_park() {
        let (blocked_tx, blocked_rx) = mpsc::channel();
        let (co_tx, co_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        // Grab the timer thread for 500ms. The 1ms timeout resumes this
        // coroutine inline on the timer thread, so the sleep below stops all
        // timer processing until it returns.
        let blocker = crate::go!(move || {
            coroutine::park_timeout(Duration::from_millis(1));
            blocked_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(500));
        });

        blocked_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("timer thread was never captured");

        // First wait: armed with a 50ms timer, ended by `unpark`. Its removal
        // is queued, but the busy timer thread cannot process it yet.
        let waiter = crate::go!(move || {
            co_tx.send(coroutine::current()).unwrap();
            coroutine::park_timeout(Duration::from_millis(50));

            // Second wait: a long timeout, so returning from it before the
            // cleanup `unpark` below can only be the stale 50ms timer.
            started_tx.send(()).unwrap();
            coroutine::park_timeout(Duration::from_secs(60));
            done_tx.send(()).unwrap();
        });

        let waiter_co = co_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        // Let the first wait park (arming its timer), then end it by `unpark`.
        // The timer cannot fire first: the timer thread is still asleep.
        std::thread::sleep(Duration::from_millis(20));
        waiter_co.unpark();

        started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("did not reach the second park");

        // The timer thread wakes up ~500ms after `blocked` and fires the now
        // expired 50ms timer. With a hand-off handle shared between the
        // waits, that timer resumes the second wait; with a per-wait handle it
        // finds its own emptied handle and does nothing.
        let early = done_rx.recv_timeout(Duration::from_millis(1500));
        assert!(
            early.is_err(),
            "a timer from the previous park woke the next wait"
        );

        // Clean up: end the long wait by `unpark`.
        waiter_co.unpark();
        done_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        waiter.join().unwrap();
        blocker.join().unwrap();
    }
}
