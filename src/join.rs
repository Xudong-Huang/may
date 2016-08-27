use std::thread;
use std::result;
use std::any::Any;
use std::sync::Arc;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use coroutine::{CoroutineImpl, Coroutine, EventSource};
use generator::is_generator;
use scheduler::get_scheduler;
use yield_now::yield_with;
use sync::AtomicOption;

pub struct Join {
    // the coroutine that waiting for this join handler
    wait_co: Option<CoroutineImpl>,
    // the flag indicate if the host coroutine is finished
    state: AtomicUsize,

    // thread talk
    wait_thread: Option<thread::Thread>,
    t_state: AtomicUsize,
}

// the state of the Join struct
const INIT: usize = 0;
const WAIT: usize = 1;
// const DONE: usize = 2;


// this is the join resource type
impl Join {
    pub fn new() -> Self {
        Join {
            wait_co: None,
            state: AtomicUsize::new(INIT),
            wait_thread: None,
            t_state: AtomicUsize::new(INIT),
        }
    }

    pub fn trigger(&mut self) {
        let state = self.state.fetch_add(1, Ordering::Relaxed);

        // when the coroutine is done, release the wait_co
        // if the old value is WAIT current value is DONE now
        // if the old value is INIT, there is no registered coroutine wait on it do nothing
        // the state can't be DONE since it can only triggered once
        if state == WAIT {
            let co = self.wait_co.take().unwrap();
            get_scheduler().schedule(co);
        }

        // wake up the waiting thread
        let t_state = self.t_state.fetch_add(1, Ordering::Relaxed);
        if t_state == WAIT {
            let th = self.wait_thread.take().unwrap();
            th.unpark();
        }
    }

    fn subscribe(&mut self, co: CoroutineImpl) {
        let state = self.state.load(Ordering::Relaxed);
        // if the state is INIT, register co to this resource, otherwise do nothing
        if state == INIT {
            // register the coroutine first
            self.wait_co = Some(co);
            // commit the state
            match self.state
                .compare_exchange(INIT, WAIT, Ordering::Release, Ordering::Relaxed) {
                Ok(_) => {
                    // successfully register the coroutine
                }
                Err(_) => {
                    // CAS failed here, it's already triggered
                    // repush the co to the ready list
                    let co = self.wait_co.take().unwrap();
                    get_scheduler().schedule(co);
                }
            }
        } else {
            // it's already trriggered
            get_scheduler().schedule(co);
        }

    }

    fn thread_wait(&mut self) {
        let state = self.t_state.load(Ordering::Relaxed);
        // if the state is INIT, register co to this resource, otherwise do nothing
        if state == INIT {
            // register the coroutine first
            self.wait_thread = Some(thread::current());
            // commit the state
            match self.t_state
                .compare_exchange(INIT, WAIT, Ordering::Release, Ordering::Relaxed) {
                Ok(_) => {
                    // successfully register the thread
                }
                Err(_) => {
                    // CAS failed here, it's already triggered
                    // repush the co to the ready list
                    let th = self.wait_thread.take().unwrap();
                    th.unpark();
                }
            }
            thread::park();
        }
    }
}

/// A join handle to a coroutine
pub struct JoinHandle<T> {
    co: Coroutine,
    join: Arc<UnsafeCell<Join>>,
    packet: Arc<AtomicOption<T>>,
}

/// create a JoinHandle
pub fn make_join_handle<T>(co: Coroutine,
                           join: Arc<UnsafeCell<Join>>,
                           packet: Arc<AtomicOption<T>>)
                           -> JoinHandle<T> {
    JoinHandle {
        co: co,
        join: join,
        packet: packet,
    }
}

pub type Result<T> = result::Result<T, Box<Any + Send + 'static>>;

impl<T> JoinHandle<T> {
    /// Extracts a handle to the underlying coroutine
    pub fn coroutine(&self) -> &Coroutine {
        &self.co
    }
    /// Join the coroutine, returning the result it produced.
    pub fn join(self) -> Result<T> {
        let join = unsafe { &mut *self.join.get() };
        if is_generator() {
            let state = join.state.load(Ordering::Relaxed);
            // if the state is not init, do nothing since the waited coroutine is done
            if state == INIT {
                yield_with(&self); // next would call subscribe in the schedule
            }
            // println!("join state: {:?}", state);
        } else {
            // this is from thread context!
            join.thread_wait();
        }
        // TODO: need to fully support Result
        Ok(self.packet.take(Ordering::Acquire).expect("take packet error!"))
    }
}

impl<T> EventSource for JoinHandle<T> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        // register the coroutine
        let join = unsafe { &mut *self.join.get() };
        join.subscribe(co);
    }
}
