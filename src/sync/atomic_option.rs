use crossbeam::atomic::AtomicCell;

use super::Blocker;
use crate::coroutine_impl::CoroutineImpl;

use std::sync::Arc;

/// A minimal atomic `Option<T>`.
///
/// Used by the synchronization primitives as a single-slot hand-off. It is
/// lock-free when `T` fits `AtomicCell`'s atomic path (asserted for the
/// crate's uses) and falls back to `AtomicCell`'s lock otherwise. All
/// operations are sequentially consistent.
pub struct AtomicOption<T> {
    inner: AtomicCell<Option<T>>,
}

const _: () = assert!(AtomicCell::<Option<CoroutineImpl>>::is_lock_free());
const _: () = assert!(AtomicCell::<Option<Arc<Blocker>>>::is_lock_free());

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

    /// Empty the slot, dropping any value.
    #[inline]
    pub fn clear(&self) {
        self.inner.store(None)
    }
}
