use std::thread;
use std::time::Duration;
use coroutine::{self, is_coroutine};

pub enum Blocker {
    Coroutine(coroutine::Coroutine),
    Thread(thread::Thread),
}

impl Blocker {
    pub fn current() -> Self {
        if is_coroutine() {
            Blocker::Coroutine(coroutine::current())
        } else {
            Blocker::Thread(thread::current())
        }
    }

    #[inline]
    pub fn park(timeout: Option<Duration>) {
        if is_coroutine() {
            timeout.map_or_else(|| coroutine::park(), |dur| coroutine::park_timeout(dur));
        } else {
            timeout.map_or_else(|| thread::park(), |dur| thread::park_timeout(dur));
        }
    }

    #[inline]
    pub fn unpark(self) {
        match self {
            Blocker::Coroutine(co) => co.unpark(),
            Blocker::Thread(t) => t.unpark(),
        }
    }
}
