use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

use generator::Generator;

/// Wrapper for a pinter type
pub trait PointerType {
    /// underlying type
    type Data;

    /// convert Self to the underlying type raw pointer
    fn into_raw(self) -> *mut Self::Data;

    /// convert Self from the underlying type raw pointer
    /// # Safety
    ///
    /// You must make sure the raw pointer comes from call of `into_raw`
    unsafe fn from_raw(_: *mut Self::Data) -> Self;
}

// impl<T> Wrapped for T {
//     default type Data = T;
//     default fn into_raw(self) -> *mut Self::Data {
//         Box::into_raw(Box::new(self)) as _
//     }
//     default unsafe fn from_raw(p: *mut Self::Data) -> T {
//         *Box::from_raw(p as _)
//     }
// }

impl<T> PointerType for *mut T {
    type Data = T;
    fn into_raw(self) -> *mut T {
        self
    }
    unsafe fn from_raw(p: *mut T) -> *mut T {
        p
    }
}

impl<T> PointerType for Arc<T> {
    type Data = T;
    fn into_raw(self) -> *mut T {
        Arc::into_raw(self) as *mut _
    }
    unsafe fn from_raw(p: *mut T) -> Arc<T> {
        Arc::from_raw(p)
    }
}

impl<T> PointerType for Box<T> {
    type Data = T;
    fn into_raw(self) -> *mut T {
        Box::into_raw(self)
    }
    unsafe fn from_raw(p: *mut T) -> Box<T> {
        Box::from_raw(p)
    }
}

impl<'a, A, T> PointerType for Generator<'a, A, T> {
    type Data = usize;
    fn into_raw(self) -> *mut usize {
        Generator::into_raw(self)
    }
    unsafe fn from_raw(p: *mut usize) -> Self {
        Generator::from_raw(p)
    }
}

#[derive(Debug)]
pub struct AtomicOption<T: PointerType> {
    inner: AtomicPtr<T::Data>,
}

unsafe impl<T: PointerType + Send> Send for AtomicOption<T> {}
unsafe impl<T: PointerType + Send> Sync for AtomicOption<T> {}

impl<T: PointerType> AtomicOption<T> {
    pub fn none() -> AtomicOption<T> {
        AtomicOption {
            inner: AtomicPtr::new(ptr::null_mut()),
        }
    }

    pub fn some(t: T) -> AtomicOption<T> {
        AtomicOption {
            inner: AtomicPtr::new(t.into_raw()),
        }
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
    pub fn is_none(&self) -> bool {
        self.inner.load(Ordering::Acquire).is_null()
    }
}

impl<T: PointerType> Default for AtomicOption<T> {
    fn default() -> Self {
        Self::none()
    }
}

impl<T: PointerType> Drop for AtomicOption<T> {
    fn drop(&mut self) {
        self.take(Ordering::Acquire);
    }
}
