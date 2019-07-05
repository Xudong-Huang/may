use crate::sync::AtomicOption;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::coroutine_impl::{co_cancel_data, is_coroutine, CoroutineImpl, EventSource};
use crate::scheduler::get_scheduler;
use crate::yield_now::{get_co_para, yield_with};

struct Sleep {
    dur: Duration,
}

impl EventSource for Sleep {
    // register the coroutine to the park
    fn subscribe(&mut self, co: CoroutineImpl) {
        let cancel = co_cancel_data(&co);
        // put the coroutine into the timer list
        let sleep_co = Arc::new(AtomicOption::some(co));
        get_scheduler().add_timer(self.dur, sleep_co.clone());

        // register the cancel data
        cancel.set_co(sleep_co);
        // re-check the cancel status
        if cancel.is_canceled() {
            #[cold]
            unsafe {
                cancel.cancel()
            };
        }
    }
}

/// block the current coroutine until timeout
pub fn sleep(dur: Duration) {
    if !is_coroutine() {
        return thread::sleep(dur);
    }

    let sleeper = Sleep { dur };
    yield_with(&sleeper);
    // consume the timeout error
    get_co_para();
}
