use std::sync::atomic::{AtomicPtr, Ordering};
use std::ptr;

// rust doesn't support template specialization now, so we create this file
pub trait Boxed {
    type Data;
    fn into_raw(self) -> *mut Self::Data;
}

impl<T> Boxed for Box<T> {
    type Data = T;
    fn into_raw(self) -> *mut T {
        Box::into_raw(self)
    }
}

#[derive(Debug)]
pub struct BoxedOption<T: Boxed> {
    inner: AtomicPtr<T::Data>,
}

unsafe impl<T: Boxed> Send for BoxedOption<T> {}
unsafe impl<T: Boxed> Sync for BoxedOption<T> {}

impl<T: Boxed> BoxedOption<T> {
    pub fn none() -> BoxedOption<T> {
        BoxedOption { inner: AtomicPtr::new(ptr::null_mut()) }
    }

    pub fn some(t: T) -> BoxedOption<T> {
        BoxedOption { inner: AtomicPtr::new(t.into_raw()) }
    }

    #[inline]
    fn swap_inner(&self, ptr: *mut T::Data, order: Ordering) -> Option<Box<T::Data>> {
        let old = self.inner.swap(ptr, order);
        if old.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(old) })
        }
    }

    #[inline]
    #[allow(dead_code)]
    pub fn swap(&self, t: T, order: Ordering) -> Option<Box<T::Data>> {
        self.swap_inner(t.into_raw(), order)
    }

    #[inline]
    #[allow(dead_code)]
    pub fn take(&self, order: Ordering) -> Option<Box<T::Data>> {
        self.swap_inner(ptr::null_mut(), order)
    }

    #[inline]
    pub fn take_fast(&self, order: Ordering) -> Option<Box<T::Data>> {
        // our special version only apply with a grab contention
        // for generic use case this is not true
        if self.is_none() {
            return None;
        }
        self.swap_inner(ptr::null_mut(), order)
    }

    #[inline]
    pub fn is_none(&self) -> bool {
        self.inner.load(Ordering::Acquire).is_null()
    }
}

impl<T: Boxed> Drop for BoxedOption<T> {
    fn drop(&mut self) {
        self.take_fast(Ordering::Relaxed);
    }
}
