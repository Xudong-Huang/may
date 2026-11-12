use crossbeam::atomic::AtomicCell;

use super::Blocker;
use crate::coroutine_impl::CoroutineImpl;
use crate::timeout_list::TimeoutHandle;

use std::sync::Arc;

/// The shared hand-off handle for one suspended coroutine.
///
/// The coroutine is taken out of it by exactly one of: an unpark, a
/// cancellation, or the scheduler timer armed for that wait. The `Arc` lets
/// the timer and the cancel machinery keep a claim on their own wait after it
/// has ended, without ever reaching a later wait registered elsewhere.
pub(crate) type WaitCo = Arc<AtomicOption<CoroutineImpl>>;

const _: () = assert!(AtomicCell::<Option<CoroutineImpl>>::is_lock_free());
const _: () = assert!(AtomicCell::<Option<Arc<Blocker>>>::is_lock_free());
const _: () = assert!(AtomicCell::<Option<WaitCo>>::is_lock_free());
// the timer handle slots of `park::Park` and `spsc::RecvTimeoutSource`
const _: () = assert!(AtomicCell::<Option<TimeoutHandle<WaitCo>>>::is_lock_free());

/// A minimal atomic `Option<T>`.
///
/// Used by the synchronization primitives as a single-slot hand-off. It is
/// lock-free when `T` fits `AtomicCell`'s atomic path (asserted for the
/// crate's uses) and falls back to `AtomicCell`'s lock otherwise. All
/// operations are sequentially consistent.
pub struct AtomicOption<T> {
    inner: AtomicCell<Option<T>>,
}

impl<T> AtomicOption<T> {
    /// Create an empty slot.
    pub const fn none() -> AtomicOption<T> {
        AtomicOption {
            inner: AtomicCell::new(None),
        }
    }

    /// Create a slot containing `t`.
    pub const fn some(t: T) -> AtomicOption<T> {
        AtomicOption {
            inner: AtomicCell::new(Some(t)),
        }
    }

    /// Store `t`, dropping any previous value.
    #[inline]
    pub fn store(&self, t: T) {
        self.inner.store(Some(t));
    }

    /// Take the value out of the slot, leaving it empty.
    #[inline]
    pub fn take(&self) -> Option<T> {
        self.inner.take()
    }

    /// Replace the value in the slot, returning the previous value.
    #[inline]
    pub fn swap(&self, t: T) -> Option<T> {
        self.inner.swap(Some(t))
    }

    /// Empty the slot, dropping any value.
    #[inline]
    pub fn clear(&self) {
        self.inner.store(None)
    }
}
