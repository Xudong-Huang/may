use std::thread;
use std::result;
use std::any::Any;
use std::sync::Arc;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use coroutine::{CoroutineImpl, Coroutine, EventSource, is_coroutine};
use scheduler::get_scheduler;
use yield_now::yield_with;
use sync::AtomicOption;

enum Waiter {
    Coroutine(CoroutineImpl),
    Thread(thread::Thread),
}

impl Waiter {
    pub fn schedule(self) {
        match self {
            Waiter::Coroutine(co) => get_scheduler().schedule(co),
            Waiter::Thread(t)=> t.unpark(),
        }
    }
}

pub struct Join {
    // the coroutine that waiting for this join handler
    waiter: AtomicOption<Waiter>,
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
            waiter: AtomicOption::none(),
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
        self.waiter.take(Ordering::Relaxed).map(|w| w.schedule());
    }

    fn coroutine_wait(&mut self) {
        // if the state is INIT, register co to this resource, otherwise do nothing
        if self.state.load(Ordering::Acquire) == INIT {
            // successfully register the coroutine
        } else {
            // it's already trriggered
            self.waiter.take(Ordering::Relaxed).map(|w| w.schedule());
        }
    }

    fn thread_wait(&mut self) {
        if self.state.load(Ordering::Relaxed) == INIT {
            // register the thread first
            self.waiter.swap(Waiter::Thread(thread::current()), Ordering::Release);
            if self.state.load(Ordering::Acquire) == INIT {
                // successfully register the thread
            } else {
                // it's already trriggered
                self.waiter.take(Ordering::Relaxed).map(|w| w.schedule());
            }
            thread::park();
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
        if is_coroutine() {
            // if the state is not init, do nothing since the waited coroutine is done
            if join.state.load(Ordering::Relaxed) == INIT {
                yield_with(&self); // next would call subscribe in the schedule
            }
            // println!("join state: {:?}", state);
        } else {
            // this is from thread context!
            join.thread_wait();
        }
        // take the result
        self.packet.take(Ordering::Acquire).ok_or_else(|| {
            let p = unsafe { &mut *self.panic.get() };
            p.take().expect("can't get panic data")
        })
    }
}

impl<T> EventSource for JoinHandle<T> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        // register the coroutine
        let join = unsafe { &mut *self.join.get() };
        join.waiter.swap(Waiter::Coroutine(co), Ordering::Release);
        join.coroutine_wait();
    }
}
