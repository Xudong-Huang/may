//! a lightweight blocker compared with `may::sync::Blocker`
//! this is not reusable, only for one time use
//! and no timeout blocking support

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{blocking::ThreadPark, AtomicOption};
use crate::coroutine_impl::{
    co_cancel_data, is_coroutine, run_coroutine, CoroutineImpl, EventSource,
};
use crate::park::ParkError;
use crate::scheduler::get_scheduler;
use crate::yield_now::{get_co_para, yield_with};

pub struct Park {
    // the coroutine that waiting for this park instance
    wait_co: Arc<AtomicOption<CoroutineImpl>>,
    // flag if parked
    state: AtomicBool,
    // the container of the park to hold resource in kernel
    container: UnsafeCell<Option<Arc<Blocker>>>,
}

// this is the park resource type (spmc style)
impl Park {
    pub fn new() -> Self {
        Park {
            wait_co: Arc::new(AtomicOption::none()),
            state: AtomicBool::new(false),
            container: UnsafeCell::new(None),
        }
    }

    /// park current coroutine
    /// if cancellation detected, return Err(ParkError::Canceled)
    pub fn park(&self, container: Arc<Blocker>) -> Result<(), ParkError> {
        if self.state.load(Ordering::Acquire) {
            return Ok(());
        }
        unsafe { *self.container.get() = Some(container) };
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
}

impl EventSource for Park {
    // register the coroutine to the park
    fn subscribe(&mut self, co: CoroutineImpl) {
        let cancel = co_cancel_data(&co);
        // delay drop the container here to hold the resource
        let _container = self.container.get_mut().take().unwrap();
        // register the coroutine
        self.wait_co.store(co);
        // re-check the state, only clear once after resume
        if self.state.load(Ordering::Acquire) {
            // fast check first
            if !self.wait_co.is_none() {
                if let Some(co) = self.wait_co.take() {
                    run_coroutine(co);
                }
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
    pub fn park(self: &Arc<Self>) -> Result<(), ParkError> {
        match self.as_ref() {
            Blocker::Coroutine(ref co) => co.park(self.clone()),
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
