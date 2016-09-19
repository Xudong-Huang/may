// comes from crossbeam
use std::sync::atomic::{AtomicPtr, Ordering};
use std::ptr;

unsafe impl<T: Send> Send for AtomicOption<T> {}
unsafe impl<T: Send> Sync for AtomicOption<T> {}

#[derive(Debug)]
pub struct AtomicOption<T> {
    inner: AtomicPtr<T>,
}

impl<T> AtomicOption<T> {
    pub fn new() -> AtomicOption<T> {
        AtomicOption { inner: AtomicPtr::new(ptr::null_mut()) }
    }

    fn swap_inner(&self, ptr: *mut T, order: Ordering) -> Option<Box<T>> {
        let old = self.inner.swap(ptr, order);
        if old.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(old) })
        }
    }

    // allows re-use of allocation
    pub fn swap_box(&self, t: Box<T>, order: Ordering) -> Option<Box<T>> {
        self.swap_inner(Box::into_raw(t), order)
    }

    #[inline]
    pub fn is_none(&self) -> bool {
        self.inner.load(Ordering::Relaxed).is_null()
    }

    pub fn swap(&self, t: T, order: Ordering) -> Option<T> {
        self.swap_box(Box::new(t), order).map(|old| *old)
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
}

impl<T> Drop for AtomicOption<T> {
    fn drop(&mut self) {
        self.take_fast(Ordering::Release);
    }
}
