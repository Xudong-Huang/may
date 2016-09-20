use std::sync::Arc;
use std::time::Duration;
use std::sync::atomic::{AtomicUsize, Ordering};
use sync::AtomicOption;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct Park {
    // the coroutine that waiting for this join handler
    wait_co: Arc<AtomicOption<CoroutineImpl>>,
    state: AtomicUsize,
    timeout: Option<Duration>,
}

// this is the park resource type (spmc style)
impl Park {
    pub fn new() -> Self {
        Park {
            wait_co: Arc::new(AtomicOption::new()),
            state: AtomicUsize::new(0),
            timeout: None,
        }
    }

    // set the timeout duration of the parking
    pub fn set_timeout(&self, dur: Option<Duration>) {
        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        me.timeout = dur;
    }

    // return true if need park the coroutine
    // when the state is 1, we clear it and indicate not to block
    // when the state is 0, means we need real park
    pub fn check_park(&self) -> bool {
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
        let s = get_scheduler();
        let sleep_co = Arc::new(AtomicOption::new());
        sleep_co.swap(co, Ordering::Relaxed);
        let timeout = self.timeout.take();
        timeout.map(|dur| {
            s.add_timer(dur, sleep_co.clone());
        });

        self.wait_co = sleep_co;
        // re-check the state
        if !self.check_park() {
            self.wait_co.take(Ordering::Relaxed).map(|co| {
                s.schedule(co);
            });
        }
    }
}
