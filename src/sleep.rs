use std::thread;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use sync::AtomicOption;
use yield_now::yield_with;
use generator::is_generator;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

struct Sleep {
    dur: Duration,
}

impl EventSource for Sleep {
    // register the coroutine to the park
    fn subscribe(&mut self, co: CoroutineImpl) {
        // put the coroutine into the timer list
        let sleep_co = Arc::new(AtomicOption::new());
        sleep_co.swap(co, Ordering::Relaxed);
        get_scheduler().add_timer(self.dur, sleep_co);
    }
}

/// block the current coroutine until timeout
pub fn sleep(dur: Duration) {
    if !is_generator() {
        return thread::sleep(dur);
    }

    let sleeper = Sleep { dur: dur };
    yield_with(&sleeper);
}
