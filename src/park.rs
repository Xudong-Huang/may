use std::sync::atomic::{AtomicUsize, Ordering};
use coroutine::{CoroutineImpl, EventSource};
use scheduler::get_scheduler;
use sync::AtomicOption;

pub struct Park {
    // the coroutine that waiting for this join handler
    wait_co: AtomicOption<CoroutineImpl>,
    state: AtomicUsize,
}

// this is the park resource type (spmc style)
impl Park {
    pub fn new() -> Self {
        Park {
            wait_co: AtomicOption::new(),
            state: AtomicUsize::new(0),
        }
    }

    // return true if need park the coroutine
    // when the state is 1, we clear it and indicate not to block
    // when the state is 0, means we need real park
    pub fn need_park(&self) -> bool {
        let state = self.state.load(Ordering::Relaxed);
        if state == 0 {
            return true;
        }

        loop {
            match self.state
                .compare_exchange_weak(1, 0, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => {
                    // successfully consume the state
                    // don't need to block
                    return false;
                }
                Err(x) => {
                    if x == 0 {
                        return true;
                    }
                }
            }
        }
    }

    // unpark the underlying coroutine if any
    pub fn unpark(&self) {
        let state = self.state.load(Ordering::Relaxed);
        if state > 0 {
            // the state is already set do nothing here
            return;
        }

        loop {
            match self.state
                .compare_exchange_weak(0, 1, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => {
                    self.wait_co.take(Ordering::Relaxed).map(|co| {
                        get_scheduler().schedule(co);
                    });
                    return;
                }
                Err(x) => {
                    if x == 1 {
                        break; // already set, do nothing
                    }
                }
            }
        }
    }
}

impl EventSource for Park {
    // register the coroutine to the park
    fn subscribe(&mut self, co: CoroutineImpl) {
        // here the co just yield, most likely it's in parking state
        // what happens here when other thread trigger the state ?
        // we would have no chance to unpark()!!!
        self.wait_co.swap(co, Ordering::Relaxed);
        // re-check the state
        if !self.need_park() {
            self.wait_co.take(Ordering::Relaxed).map(|co| {
                get_scheduler().schedule(co);
            });
        }
    }
}
