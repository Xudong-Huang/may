//! a lightweight blocker compared with `may::sync::Blocker`
//! this is not reusable, only for one time use
//! and no timeout blocking support

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{blocking::ThreadPark, AtomicOption};
use crate::coroutine_impl::{
    co_cancel_data, is_coroutine, run_coroutine, CoroutineImpl, EventSource,
};
use crate::park::ParkError;
use crate::scheduler::get_scheduler;
use crate::yield_now::{get_co_para, yield_now, yield_with};

pub struct Park {
    // the coroutine that waiting for this park instance
    wait_co: Arc<AtomicOption<CoroutineImpl>>,
    // flag if parked
    state: AtomicBool,
    // a flag if kernel is entered
    wait_kernel: AtomicBool,
}

impl Drop for Park {
    fn drop(&mut self) {
        // wait the kernel finish
        while self.wait_kernel.load(Ordering::Relaxed) {
            yield_now();
        }
    }
}

pub struct DropGuard<'a>(&'a Park);

impl<'a> Drop for DropGuard<'a> {
    fn drop(&mut self) {
        self.0.wait_kernel.store(false, Ordering::Relaxed);
    }
}

// this is the park resource type (spmc style)
impl Park {
    pub fn new() -> Self {
        Park {
            wait_co: Arc::new(AtomicOption::none()),
            state: AtomicBool::new(false),
            wait_kernel: AtomicBool::new(false),
        }
    }

    /// park current coroutine
    /// if cancellation detected, return Err(ParkError::Canceled)
    pub fn park(&self) -> Result<(), ParkError> {
        if self.state.load(Ordering::Acquire) {
            return Ok(());
        }
        yield_with(self);

        if let Some(err) = get_co_para() {
            match err.kind() {
                std::io::ErrorKind::Other => return Err(ParkError::Canceled),
                std::io::ErrorKind::TimedOut => return Err(ParkError::Timeout),
                _ => unreachable!("unexpected return error kind"),
            }
        }

        Ok(())
    }

    // unpark the underlying coroutine if any, push to the ready task queue
    #[inline]
    pub fn unpark(&self) {
        self.state.store(true, Ordering::Release);
        if let Some(co) = self.wait_co.take() {
            get_scheduler().schedule(co);
        }
    }

    fn delay_drop(&self) -> DropGuard {
        self.wait_kernel.store(true, Ordering::Relaxed);
        DropGuard(self)
    }
}

impl EventSource for Park {
    // register the coroutine to the park
    fn subscribe(&mut self, co: CoroutineImpl) {
        let cancel = co_cancel_data(&co);
        let _g = self.delay_drop();
        // register the coroutine
        self.wait_co.store(co);
        // re-check the state, only clear once after resume
        if self.state.load(Ordering::Acquire) {
            if let Some(co) = self.wait_co.take() {
                run_coroutine(co);
            }
            return;
        }

        // register the cancel data
        cancel.set_co(self.wait_co.clone());
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}

pub enum Blocker {
    Coroutine(Park),
    Thread(ThreadPark),
}

impl Blocker {
    /// create a new fast blocker
    pub fn new() -> Arc<Self> {
        let blocker = if is_coroutine() {
            Blocker::Coroutine(Park::new())
        } else {
            Blocker::Thread(ThreadPark::new())
        };
        Arc::new(blocker)
    }

    #[inline]
    pub fn park(&self) -> Result<(), ParkError> {
        match self {
            Blocker::Coroutine(ref co) => co.park(),
            Blocker::Thread(ref t) => t.park_timeout(None),
        }
    }

    #[inline]
    pub fn unpark(&self) {
        match self {
            Blocker::Coroutine(ref co) => co.unpark(),
            Blocker::Thread(ref t) => t.unpark(),
        }
    }
}
