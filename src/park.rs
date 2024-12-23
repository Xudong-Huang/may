use std::fmt;
use std::io::ErrorKind;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::cancel::Cancel;
use crate::coroutine_impl::{co_cancel_data, run_coroutine, CoroutineImpl, EventSource};
use crate::scheduler::get_scheduler;
use crate::sync::atomic_dur::AtomicDuration;
use crate::sync::AtomicOption;
use crate::timeout_list::TimeoutHandle;
use crate::yield_now::{get_co_para, yield_now, yield_with};

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ParkError {
    Canceled,
    Timeout,
}

pub struct DropGuard<'a>(&'a Park);
pub struct Park {
    // the coroutine that waiting for this park instance
    wait_co: Arc<AtomicOption<CoroutineImpl>>,
    // when true means the Park no need to block
    state: AtomicBool,
    // control how to deal with the cancellation, usually init one time
    check_cancel: AtomicBool,
    // timeout settings in ms, 0 is none (park forever)
    timeout: AtomicDuration,
    // timer handle, can be null
    timeout_handle: AtomicPtr<TimeoutHandle<Arc<AtomicOption<CoroutineImpl>>>>,
    // a flag if kernel is entered
    wait_kernel: AtomicBool,
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
            wait_co: Arc::new(AtomicOption::none()),
            state: AtomicBool::new(false),
            check_cancel: AtomicBool::new(true),
            timeout: AtomicDuration::new(None),
            timeout_handle: AtomicPtr::new(ptr::null_mut()),
            wait_kernel: AtomicBool::new(false),
        }
    }

    // ignore cancel, if true, caller have to do the check instead
    pub fn ignore_cancel(&self, ignore: bool) {
        self.check_cancel.store(!ignore, Ordering::Relaxed);
    }

    #[inline]
    fn set_timeout_handle(
        &self,
        handle: Option<TimeoutHandle<Arc<AtomicOption<CoroutineImpl>>>>,
    ) -> Option<TimeoutHandle<Arc<AtomicOption<CoroutineImpl>>>> {
        let ptr = match handle {
            None => ptr::null_mut(),
            Some(h) => h.into_ptr(),
        };

        let old_ptr = self.timeout_handle.swap(ptr, Ordering::Relaxed);
        if old_ptr.is_null() {
            None
        } else {
            Some(unsafe { TimeoutHandle::from_ptr(old_ptr) })
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
        if let Some(co) = self.wait_co.take() {
            if b_sync {
                run_coroutine(co);
            } else {
                get_scheduler().schedule(co);
            }
        }
    }

    #[inline]
    fn fast_wake_up(&self) {
        if let Some(co) = self.wait_co.take() {
            run_coroutine(co);
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
        while self.wait_kernel.load(Ordering::Acquire) {
            yield_now();
        }

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

    fn delay_drop(&self) -> DropGuard {
        self.wait_kernel.store(true, Ordering::Release);
        DropGuard(self)
    }
}

impl Drop for DropGuard<'_> {
    fn drop(&mut self) {
        self.0.wait_kernel.store(false, Ordering::Release);
    }
}

impl Drop for Park {
    fn drop(&mut self) {
        // wait the kernel finish
        while self.wait_kernel.load(Ordering::Acquire) {
            yield_now();
        }

        self.set_timeout_handle(None);
    }
}

impl EventSource for Park {
    // register the coroutine to the park
    fn subscribe(&mut self, co: CoroutineImpl) {
        let cancel = co_cancel_data(&co);
        // if we share the same park, the previous timer may wake up it by false
        // if we not deleted the timer in time
        let timeout_handle = self
            .timeout
            .take()
            .map(|dur| get_scheduler().add_timer(dur, self.wait_co.clone()));
        self.set_timeout_handle(timeout_handle);

        let _g = self.delay_drop();

        // register the coroutine
        self.wait_co.store(co);

        // re-check the state, only clear once after resume
        if self.state.load(Ordering::Acquire) {
            // here may have recursive call for subscribe
            // normally the recursion depth is not too deep
            return self.fast_wake_up();
        }

        // register the cancel data
        cancel.set_co(self.wait_co.clone());
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
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
