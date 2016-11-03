use std::sync::Arc;
use std::time::Duration;
use std::sync::atomic::{AtomicUsize, Ordering};
use sync::BoxedOption;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct Park {
    // the coroutine that waiting for this join handler
    wait_co: Arc<BoxedOption<CoroutineImpl>>,
    // when odd means the Park no need to block
    // the low bit used as flag, and higher bits used as tag to prevent ABA problem
    state: AtomicUsize,
    timeout: Option<Duration>,
}

// this is the park resource type (spmc style)
impl Park {
    pub fn new() -> Self {
        Park {
            wait_co: Arc::new(BoxedOption::none()),
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
    // when the state is true, we clear it and indicate not to block
    // when the state is false, means we need real park
    pub fn check_park(&self) -> bool {
        let mut state = self.state.load(Ordering::Acquire);
        if state & 1 == 0 {
            return true;
        }

        loop {
            match self.state
                .compare_exchange_weak(state, state + 1, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => return false, // successfully consume the state, no need to block
                Err(x) if x & 1 == 0 => return true,
                Err(y) => state = y,
            }
        }
    }

    // unpark the underlying coroutine if any
    pub fn unpark(&self) {
        let mut state = self.state.load(Ordering::Acquire);
        if state & 1 == 1 {
            // the state is already set do nothing here
            return;
        }

        loop {
            match self.state
                .compare_exchange_weak(state, state + 1, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => return self.wake_up(),
                Err(x) if x & 1 == 1 => break, // already set, do nothing
                Err(y) => state = y,
            }
        }
    }

    #[inline]
    fn wake_up(&self) {
        // ::std::sync::atomic::fence(Ordering::SeqCst);
        self.wait_co
            .take_fast(Ordering::Acquire)
            .map(|co| get_scheduler().schedule(co));
    }
}

impl EventSource for Park {
    // register the coroutine to the park
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // register the coroutine
        self.wait_co.swap(co, Ordering::Release);
        let co = self.wait_co.clone();
        self.timeout.take().map(|dur| s.add_timer(dur, co));

        // re-check the state, only clear once after resume
        if self.state.load(Ordering::Acquire) & 1 == 1 {
            self.wake_up();
        }
    }
}
