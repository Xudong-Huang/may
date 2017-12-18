//! modified from std::sys_common::poison except for both thread and coroutine
//! please ref the doc and comments from std::sys_common::poison

use std::thread;
use std::sync::{LockResult, PoisonError};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Flag {
    failed: AtomicUsize,
}

impl Flag {
    pub fn new() -> Flag {
        Flag { failed: AtomicUsize::new(0) }
    }

    #[inline]
    pub fn borrow(&self) -> LockResult<Guard> {
        let ret = Guard { panicking: thread::panicking() };
        if self.get() {
            Err(PoisonError::new(ret))
        } else {
            Ok(ret)
        }
    }

    #[inline]
    pub fn done(&self, guard: &Guard) {
        if !guard.panicking && thread::panicking() {
            self.failed.store(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn get(&self) -> bool {
        self.failed.load(Ordering::Relaxed) != 0
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
