use std::sync::atomic::{AtomicBool, Ordering};
use yield_now::yield_now;

pub struct DropGuard<'a>(&'a DelayDrop);
pub struct DelayDrop {
    // can't alloc in stack because it may moved
    // can_drop.0 is the flag that when kernel is done
    // can_drop.1 is the flag that when kernel is started
    can_drop: Box<(AtomicBool, AtomicBool)>,
}

impl DelayDrop {
    pub fn new() -> Self {
        DelayDrop {
            can_drop: Box::new((AtomicBool::new(false), AtomicBool::new(false))),
        }
    }

    pub fn delay_drop(&self) -> DropGuard {
        self.can_drop.1.store(true, Ordering::Release);
        DropGuard(self)
    }

    // windows platform doesn't use this method
    #[allow(dead_code)]
    pub fn reset(&self) {
        // wait the kernel finished
        if !self.can_drop.1.load(Ordering::Acquire) {
            while !self.can_drop.0.load(Ordering::Acquire) {
                yield_now();
            }
        }

        self.can_drop.0.store(false, Ordering::Release);
        self.can_drop.1.store(false, Ordering::Release);
    }
}

impl<'a> Drop for DropGuard<'a> {
    fn drop(&mut self) {
        // kernel would set it to true
        self.0.can_drop.0.store(true, Ordering::Release);
    }
}

impl Drop for DelayDrop {
    fn drop(&mut self) {
        // wait for drop
        if !self.can_drop.1.load(Ordering::Acquire) {
            return;
        }

        while !self.can_drop.0.load(Ordering::Acquire) {
            yield_now();
        }
    }
}
