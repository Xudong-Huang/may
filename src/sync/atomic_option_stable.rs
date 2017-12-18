use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

// heap based wrapper for a type
pub trait Wrapped {
    type Data;
    fn into_raw(self) -> *mut Self::Data;
    unsafe fn from_raw(*mut Self::Data) -> Self;
}

impl<T> Wrapped for T {
    type Data = T;
    fn into_raw(self) -> *mut Self::Data {
        Box::into_raw(Box::new(self)) as _
    }
    unsafe fn from_raw(p: *mut Self::Data) -> T {
        *Box::from_raw(p as _)
    }
}

#[derive(Debug)]
pub struct AtomicOption<T: Wrapped> {
    inner: AtomicPtr<T::Data>,
}

unsafe impl<T: Send> Send for AtomicOption<T> {}
unsafe impl<T: Send> Sync for AtomicOption<T> {}

impl<T: Wrapped> AtomicOption<T> {
    pub fn none() -> AtomicOption<T> {
        AtomicOption { inner: AtomicPtr::new(ptr::null_mut()) }
    }

    pub fn some(t: T) -> AtomicOption<T> {
        AtomicOption { inner: AtomicPtr::new(t.into_raw()) }
    }

    #[inline]
    fn swap_inner(&self, ptr: *mut T::Data, order: Ordering) -> Option<T> {
        let old = self.inner.swap(ptr, order);
        if old.is_null() {
            None
        } else {
            Some(unsafe { T::from_raw(old) })
        }
    }

    #[inline]
    pub fn swap(&self, t: T, order: Ordering) -> Option<T> {
        self.swap_inner(t.into_raw(), order)
    }

    #[inline]
    pub fn take(&self, order: Ordering) -> Option<T> {
        self.swap_inner(ptr::null_mut(), order)
    }

    #[inline]
    pub fn take_fast(&self, order: Ordering) -> Option<T> {
        // our special verion only apply with a grab contention
        // for generic use case this is not ture
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

impl<T: Wrapped> Drop for AtomicOption<T> {
    fn drop(&mut self) {
        self.take_fast(Ordering::Relaxed);
    }
}
