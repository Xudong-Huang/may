use std::any::Any;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::Result;

use crate::coroutine_impl::Coroutine;
use crate::sync::{AtomicOption, Blocker};
use crossbeam::atomic::AtomicCell;
use generator::Error;

pub struct Join {
    // the coroutine that waiting for this join handler
    to_wake: AtomicOption<Arc<Blocker>>,
    // the flag indicate if the host coroutine is not finished
    // when set to false, the coroutine is done
    state: AtomicBool,

    // use to set the panic err
    // this is the only place that could set the panic Error
    // we use to communicate with JoinHandle so that can return the panic info
    // this must be ready before the trigger
    panic: Arc<AtomicCell<Option<Box<dyn Any + Send>>>>,
}

// this is the join resource type
impl Join {
    pub fn new(panic: Arc<AtomicCell<Option<Box<dyn Any + Send>>>>) -> Self {
        Join {
            to_wake: AtomicOption::none(),
            state: AtomicBool::new(true),
            panic,
        }
    }

    // the the panic for the coroutine
    pub fn set_panic_data(&self, panic: Box<dyn Any + Send>) {
        self.panic.swap(Some(panic));
    }

    pub fn trigger(&self) {
        self.state.store(false, Ordering::Release);
        if let Some(w) = self.to_wake.take(Ordering::Acquire) {
            w.unpark();
        }
    }

    fn wait(&self) {
        if self.state.load(Ordering::Acquire) {
            let cur = Blocker::current();
            // register the blocker first
            self.to_wake.swap(cur.clone(), Ordering::Release);
            // re-check the state
            if self.state.load(Ordering::Acquire) {
                // successfully register the blocker
                cur.park(None).ok();
            } else {
                self.to_wake.take(Ordering::Acquire);
            }
        }
    }
}

/// A join handle to a coroutine
pub struct JoinHandle<T> {
    co: Coroutine,
    join: Arc<Join>,
    packet: Arc<AtomicCell<Option<T>>>,
    panic: Arc<AtomicCell<Option<Box<dyn Any + Send>>>>,
}

unsafe impl<T: Send> Send for JoinHandle<T> {}
unsafe impl<T: Sync> Sync for JoinHandle<T> {}

/// create a JoinHandle
pub fn make_join_handle<T>(
    co: Coroutine,
    join: Arc<Join>,
    packet: Arc<AtomicCell<Option<T>>>,
    panic: Arc<AtomicCell<Option<Box<dyn Any + Send>>>>,
) -> JoinHandle<T> {
    JoinHandle {
        co,
        join,
        packet,
        panic,
    }
}

impl<T> JoinHandle<T> {
    /// Extracts a handle to the underlying coroutine
    pub fn coroutine(&self) -> &Coroutine {
        &self.co
    }

    /// return true if the coroutine is finished
    pub fn is_done(&self) -> bool {
        !self.join.state.load(Ordering::Acquire)
    }

    /// block until the coroutine is done
    pub fn wait(&self) {
        self.join.wait();
    }

    /// Join the coroutine, returning the result it produced.
    pub fn join(self) -> Result<T> {
        self.join.wait();

        // take the result
        self.packet
            .take()
            .ok_or_else(|| self.panic.take().unwrap_or_else(|| Box::new(Error::Cancel)))
    }
}

impl<T> fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad("JoinHandle { .. }")
    }
}
