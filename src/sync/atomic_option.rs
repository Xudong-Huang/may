use crossbeam::atomic::AtomicCell;

use super::Blocker;
use crate::coroutine_impl::CoroutineImpl;

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
    pub fn swap(&self, t: T) -> Option<T> {
        self.inner.swap(Some(t))
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
    pub fn is_none(&self) -> bool {
        unsafe { (*self.inner.as_ptr()).is_none() }
    }
}
