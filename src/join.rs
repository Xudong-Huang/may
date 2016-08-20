use std::sync::Arc;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use coroutine::{Coroutine, EventSource};
use scheduler::get_scheduler;
use yield_now::_yield_with;

pub struct Join {
    // the coroutine that waiting for this join handler
    wait_co: Option<Coroutine>,
    // the flag indicate if the host coroutine is finished
    state: AtomicUsize,
}

// the state of the Join struct
const INIT: usize = 0;
const WAIT: usize = 1;
// const DONE: usize = 2;

impl Join {
    pub fn new() -> Self {
        Join {
            wait_co: None,
            state: AtomicUsize::new(INIT),
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
                Err(..) => {
                    // CAS failed here, it's already triggered
                    // repush the co to the ready list
                    let co = self.wait_co.take().unwrap();
                    get_scheduler().schedule(co);
                }
            }
        }
    }
}

pub struct JoinHandler(pub Arc<UnsafeCell<Join>>);

impl JoinHandler {
    // TODO: here should return a result?
    pub fn join(self) {
        let join = unsafe { &mut *self.0.get() };
        let state = join.state.load(Ordering::Relaxed);
        // if the state is not init, do nothing since the waited coroutine is done
        if state == INIT {
            _yield_with(&self);
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
