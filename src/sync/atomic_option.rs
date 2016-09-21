use std::sync::atomic::{AtomicPtr, Ordering};
use std::ptr;

#[derive(Debug)]
pub struct AtomicOption<T> {
    inner: AtomicPtr<T>,
}

unsafe impl<T: Send> Send for AtomicOption<T> {}
unsafe impl<T: Send> Sync for AtomicOption<T> {}

impl<T> AtomicOption<T> {
    pub fn none() -> AtomicOption<T> {
        AtomicOption { inner: AtomicPtr::new(ptr::null_mut()) }
    }

    pub fn some(t: T) -> AtomicOption<T> {
        AtomicOption { inner: AtomicPtr::new(Box::into_raw(Box::new(t))) }
    }

    fn swap_inner(&self, ptr: *mut T, order: Ordering) -> Option<Box<T>> {
        let old = self.inner.swap(ptr, order);
        if old.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(old) })
        }
    }

    #[inline]
    pub fn swap(&self, t: T, order: Ordering) -> Option<T> {
        self.swap_inner(Box::into_raw(Box::new(t)), order).map(|old| *old)
    }

    #[inline]
    pub fn take(&self, order: Ordering) -> Option<T> {
        self.swap_inner(ptr::null_mut(), order).map(|old| *old)
    }

    #[inline]
    pub fn take_fast(&self, order: Ordering) -> Option<T> {
        // our special verion only apply with a grab contention
        // for generic use case this is not ture
        if self.is_none() {
            return None;
        }
        self.swap_inner(ptr::null_mut(), order).map(|old| *old)
    }

    #[inline]
    pub fn is_none(&self) -> bool {
        self.inner.load(Ordering::Acquire).is_null()
    }
}

impl<T> Drop for AtomicOption<T> {
    fn drop(&mut self) {
        self.take_fast(Ordering::Relaxed);
    }
}
