use std::thread;
use std::sync::Arc;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use coroutine::{Coroutine, EventSource};
use generator::get_context;
use scheduler::get_scheduler;
use yield_now::yield_with;

pub struct Join {
    // the coroutine that waiting for this join handler
    wait_co: Option<Coroutine>,
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

    fn subscribe(&mut self, co: Coroutine) {
        let state = self.state.load(Ordering::Relaxed);
        // if the state is INIT, register co to this resource, otherwise do nothing
        if state == INIT {
            // register the coroutine first
            self.wait_co = Some(co);
            // commit the state
            match self.state
                .compare_exchange_weak(INIT, WAIT, Ordering::Release, Ordering::Relaxed) {
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
                .compare_exchange_weak(INIT, WAIT, Ordering::Release, Ordering::Relaxed) {
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

// Join resource wrapper
pub struct JoinHandler(pub Arc<UnsafeCell<Join>>);

impl JoinHandler {
    // TODO: here should return a result?
    pub fn join(self) {
        let join = unsafe { &mut *self.0.get() };
        if get_context().is_generator() {
            let state = join.state.load(Ordering::Relaxed);
            // if the state is not init, do nothing since the waited coroutine is done
            if state == INIT {
                yield_with(&self);
            }
        } else {
            // this is from thread context!
            join.thread_wait();
        }

    }
}

impl EventSource for JoinHandler {
    fn subscribe(&mut self, co: Coroutine) {
        // register the coroutine
        let join = unsafe { &mut *self.0.get() };
        join.subscribe(co);
    }
}
