use std::result;
use std::any::Any;
use std::sync::Arc;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use coroutine::Coroutine;
use sync::{Blocker, AtomicOption};

pub struct Join {
    // the coroutine that waiting for this join handler
    to_wake: AtomicOption<Blocker>,
    // the flag indicate if the host coroutine is finished
    state: AtomicUsize,

    // use to set the panic err
    // this is the only place that could set the panic Error
    // we use to communicate with JoinHandle so that can return the panic info
    // this must be ready before the trigger
    panic: Arc<UnsafeCell<Option<Box<Any + Send>>>>,
}

// the init state of the Join struct
const INIT: usize = 0;


// this is the join resource type
impl Join {
    pub fn new(panic: Arc<UnsafeCell<Option<Box<Any + Send>>>>) -> Self {
        Join {
            to_wake: AtomicOption::none(),
            state: AtomicUsize::new(INIT),
            panic: panic,
        }
    }

    // the the panic for the coroutine
    pub fn set_panic_data(&mut self, panic: Box<Any + Send>) {
        let p = unsafe { &mut *self.panic.get() };
        *p = Some(panic);
    }

    pub fn trigger(&mut self) {
        self.state.fetch_add(1, Ordering::Release);
        self.to_wake.take(Ordering::Relaxed).map(|w| w.unpark());
    }

    fn wait(&mut self) {
        if self.state.load(Ordering::Relaxed) == INIT {
            // register the blocker first
            self.to_wake.swap(Blocker::new(), Ordering::Release);
            // re-check the state
            if self.state.load(Ordering::Acquire) == INIT {
                // successfully register the blocker
            } else {
                // it's already trriggered
                self.to_wake.take(Ordering::Relaxed).map(|w| w.unpark());
            }
            Blocker::park(None);
        }
    }
}

pub type Result<T> = result::Result<T, Box<Any + Send>>;

/// A join handle to a coroutine
pub struct JoinHandle<T> {
    co: Coroutine,
    join: Arc<UnsafeCell<Join>>,
    packet: Arc<AtomicOption<T>>,
    panic: Arc<UnsafeCell<Option<Box<Any + Send>>>>,
}

/// create a JoinHandle
pub fn make_join_handle<T>(co: Coroutine,
                           join: Arc<UnsafeCell<Join>>,
                           packet: Arc<AtomicOption<T>>,
                           panic: Arc<UnsafeCell<Option<Box<Any + Send>>>>)
                           -> JoinHandle<T> {
    JoinHandle {
        co: co,
        join: join,
        packet: packet,
        panic: panic,
    }
}

impl<T> JoinHandle<T> {
    /// Extracts a handle to the underlying coroutine
    pub fn coroutine(&self) -> &Coroutine {
        &self.co
    }

    /// Join the coroutine, returning the result it produced.
    pub fn join(self) -> Result<T> {
        let join = unsafe { &mut *self.join.get() };
        join.wait();

        // take the result
        self.packet.take(Ordering::Acquire).ok_or_else(|| {
            let p = unsafe { &mut *self.panic.get() };
            p.take().expect("can't get panic data")
        })
    }
}
