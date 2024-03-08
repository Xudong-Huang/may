use crossbeam::atomic::AtomicCell;

use super::Blocker;
use crate::coroutine_impl::CoroutineImpl;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub struct AtomicOption<T> {
    inner: AtomicCell<Option<T>>,
}

const _: () = assert!(AtomicCell::<Option<CoroutineImpl>>::is_lock_free());
const _: () = assert!(AtomicCell::<Option<Arc<Blocker>>>::is_lock_free());

impl<T> AtomicOption<T> {
    pub const fn none() -> AtomicOption<T> {
        AtomicOption {
            inner: AtomicCell::new(None),
        }
    }

    pub const fn some(t: T) -> AtomicOption<T> {
        AtomicOption {
            inner: AtomicCell::new(Some(t)),
        }
    }

    #[inline]
    pub fn store(&self, t: T) {
        self.inner.store(Some(t));
    }

    /// store a value from None
    /// # Safety
    /// 1. you must make sure the `Option<T>` is_lock_free store
    /// 2. the underlying data must be None
    #[inline]
    pub unsafe fn unsync_store(&self, t: T) {
        debug_assert!(AtomicCell::<Option<T>>::is_lock_free());
        let this = self.inner.as_ptr();
        let a = unsafe { &*(this as *const _ as *const AtomicUsize) };
        unsafe { a.store(std::mem::transmute_copy(&t), Ordering::SeqCst) };
        std::mem::forget(t);
    }

    #[inline]
    pub fn take(&self) -> Option<T> {
        self.inner.take()
    }

    #[inline]
    pub fn clear(&self) {
        self.inner.store(None)
    }
}
