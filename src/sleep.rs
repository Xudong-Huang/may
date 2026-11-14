use crate::sync::AtomicOption;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::coroutine_impl::{co_cancel_data, is_coroutine, CoroutineImpl, EventSource};
use crate::likely::unlikely;
use crate::scheduler::get_scheduler;
use crate::yield_now::{get_co_para, yield_with};

struct Sleep {
    dur: Duration,
}

impl EventSource for Sleep {
    // register the coroutine to the park
    fn subscribe(&mut self, co: CoroutineImpl) {
        let cancel = co_cancel_data(&co);
        let sleep_co = Arc::new(AtomicOption::some(co));

        // Register for cancellation before arming the timer. Once the timer is
        // armed it can fire at once and resume the coroutine, which may start
        // its next wait; registering after that would leave a stale
        // registration behind and a later cancel could miss the new wait.
        cancel.set_co(sleep_co.clone());
        if cancel.is_canceled() {
            // the cancel machinery took the coroutine, so no timer is needed
            cancel.cancel();
            return;
        }

        // put the coroutine into the timer list
        get_scheduler().add_timer(self.dur, sleep_co);
    }
}

/// block the current coroutine until timeout
pub fn sleep(dur: Duration) {
    if unlikely(!is_coroutine()) {
        return thread::sleep(dur);
    }

    let sleeper = Sleep { dur };
    yield_with(&sleeper);
    // consume the timeout error
    get_co_para();
}
