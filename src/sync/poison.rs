//! modified from std::sys_common::poison except for both thread and coroutine
//! please ref the doc and comments from std::sys_common::poison

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LockResult, PoisonError};
use std::thread;

pub struct Flag {
    failed: AtomicUsize,
}

impl Flag {
    pub const fn new() -> Flag {
        Flag {
            failed: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn borrow(&self) -> LockResult<Guard> {
        let ret = Guard {
            panicking: thread::panicking(),
        };
        if self.get() {
            Err(PoisonError::new(ret))
        } else {
            Ok(ret)
        }
    }

    #[inline]
    pub fn done(&self, guard: &Guard) {
        // `thread::panicking()` is true while any unwind is in flight on this
        // thread — including a *suspended* coroutine Cancel-panic unwind,
        // during which other coroutines keep running on the thread. In that
        // window their guards must not poison: the "panic" is not theirs and
        // not a real panic at all.
        if !guard.panicking
            && thread::panicking()
            && !crate::cancel::cancel_panic_in_flight()
        {
            let is_canceled = if crate::coroutine_impl::is_coroutine() {
                let cancel = crate::coroutine_impl::current_cancel_data();
                cancel.is_canceled()
            } else {
                false
            };

            if !is_canceled {
                self.failed.store(1, Ordering::Relaxed);
            }
        }
    }

    #[inline]
    pub fn get(&self) -> bool {
        self.failed.load(Ordering::Relaxed) != 0
    }

    // clear the poisoned state
    #[inline]
    pub fn clear(&self) {
        self.failed.store(0, Ordering::Relaxed);
    }
}

pub struct Guard {
    panicking: bool,
}

pub fn map_result<T, U, F>(result: LockResult<T>, f: F) -> LockResult<U>
where
    F: FnOnce(T) -> U,
{
    match result {
        Ok(t) => Ok(f(t)),
        Err(guard) => Err(PoisonError::new(f(guard.into_inner()))),
    }
}
