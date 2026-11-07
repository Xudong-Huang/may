use crate::yield_now::yield_now;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Clears the "kernel is running" marker when the guarded scope ends.
pub struct DropGuard<'a>(&'a DelayDrop);

/// Keeps a stack-resident wait source alive until its `subscribe` kernel has
/// finished.
///
/// Call [`delay_drop`](Self::delay_drop) on entry to the kernel and drop the
/// returned guard on exit. Dropping the `DelayDrop` (e.g. from a concurrent
/// resume of the coroutine) waits until the kernel has finished, so the
/// source and the coroutine's local data are not torn down under `subscribe`.
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

    pub fn delay_drop(&self) -> DropGuard<'_> {
        self.can_drop.store(2, Ordering::Release);
        DropGuard(self)
    }

    /// Wait until the kernel (`delay_drop` guard) has finished.
    ///
    /// Safe to call from both the kernel owner and a concurrent `Drop`.
    pub fn wait(&self) {
        while self.can_drop.load(Ordering::Acquire) == 2 {
            yield_now();
        }
    }
}

impl Drop for DropGuard<'_> {
    fn drop(&mut self) {
        // kernel would set it to true
        self.0.can_drop.fetch_and(1, Ordering::Release);
    }
}

impl Drop for DelayDrop {
    fn drop(&mut self) {
        // wait for drop
        self.wait();
    }
}
