use crate::yield_now::yield_now;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct DropGuard<'a>(&'a DelayDrop);
pub struct DelayDrop {
    // can_drop & 0x1 is the flag that when kernel is done
    // can_drop & 0x2 is the flag that when kernel is started
    can_drop: AtomicUsize,
}

impl DelayDrop {
    pub fn new() -> Self {
        DelayDrop {
            can_drop: AtomicUsize::new(0),
        }
    }

    pub fn delay_drop(&self) -> DropGuard {
        self.can_drop.store(2, Ordering::Release);
        DropGuard(self)
    }

    #[allow(dead_code)]
    #[inline]
    pub fn reset(&self) {
        // wait the kernel finished
        while self.can_drop.load(Ordering::Acquire) == 2 {
            #[cold]
            yield_now();
        }

        self.can_drop.store(0, Ordering::Release);
    }
}

impl<'a> Drop for DropGuard<'a> {
    fn drop(&mut self) {
        // kernel would set it to true
        self.0.can_drop.fetch_and(1, Ordering::Release);
    }
}

impl Drop for DelayDrop {
    fn drop(&mut self) {
        // wait for drop
        while self.can_drop.load(Ordering::Acquire) == 2 {
            #[cold]
            yield_now();
        }
    }
}
