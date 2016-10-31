use std::thread;
use std::time::Duration;
use coroutine::{self, is_coroutine};

pub enum Waiter {
    Coroutine(coroutine::Coroutine),
    Thread(thread::Thread),
}

impl Waiter {
    pub fn new() -> Self {
        if is_coroutine() {
            Waiter::Coroutine(coroutine::current())
        } else {
            Waiter::Thread(thread::current())
        }
    }

    #[inline]
    pub fn park() {
        if is_coroutine() {
            coroutine::park()
        } else {
            thread::park()
        }
    }

    #[inline]
    pub fn park_timeout(dur: Duration) {
        if is_coroutine() {
            coroutine::park_timeout(dur);
        } else {
            thread::park_timeout(dur);
        }
    }

    #[inline]
    pub fn unpark(self) {
        match self {
            Waiter::Coroutine(co) => co.unpark(),
            Waiter::Thread(t) => t.unpark(),
        }
    }
}
