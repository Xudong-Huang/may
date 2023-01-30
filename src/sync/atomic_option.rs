use crossbeam::atomic::AtomicCell;

use super::Blocker;
use crate::coroutine_impl::CoroutineImpl;

use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

pub struct AtomicOption<T> {
    inner: AtomicCell<Option<T>>,
}

const _: () = assert!(AtomicCell::<Option<CoroutineImpl>>::is_lock_free());
const _: () = assert!(AtomicCell::<Option<Arc<Blocker>>>::is_lock_free());

impl<T> AtomicOption<T> {
    pub fn none() -> AtomicOption<T> {
        AtomicOption {
            inner: AtomicCell::new(None),
        }
    }

    pub fn some(t: T) -> AtomicOption<T> {
        AtomicOption {
            inner: AtomicCell::new(Some(t)),
        }
    }

    #[inline]
    pub fn store(&self, t: T) {
        self.inner.store(Some(t))
    }

    #[inline]
    pub fn take(&self) -> Option<T> {
        self.inner.take()
    }

    #[inline]
    pub fn clear(&self) {
        self.inner.store(None)
    }

    #[inline]
    pub fn is_none(&self) -> bool {
        let this = self.inner.as_ptr();
        let a = unsafe { &*(this as *const _ as *const AtomicPtr<T>) };
        a.load(Ordering::Acquire).is_null()
    }
}
