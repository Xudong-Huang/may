use std::cell::UnsafeCell;
use std::fmt;
use std::ops::Deref;

// /// `AtomicU32` providing an additional `unsync_load` function.
// pub(crate) struct AtomicU32 {
//     inner: UnsafeCell<std::sync::atomic::AtomicU32>,
// }

// unsafe impl Send for AtomicU32 {}
// unsafe impl Sync for AtomicU32 {}

// impl AtomicU32 {
//     pub(crate) const fn new(val: u32) -> AtomicU32 {
//         let inner = UnsafeCell::new(std::sync::atomic::AtomicU32::new(val));
//         AtomicU32 { inner }
//     }

//     /// Performs an unsynchronized load.
//     ///
//     /// # Safety
//     ///
//     /// All mutations must have happened before the unsynchronized load.
//     /// Additionally, there must be no concurrent mutations.
//     pub(crate) unsafe fn unsync_load(&self) -> u32 {
//         core::ptr::read(self.inner.get() as *const u32)
//     }
// }

// impl Deref for AtomicU32 {
//     type Target = std::sync::atomic::AtomicU32;

//     fn deref(&self) -> &Self::Target {
//         // safety: it is always safe to access `&self` fns on the inner value as
//         // we never perform unsafe mutations.
//         unsafe { &*self.inner.get() }
//     }
// }

// impl fmt::Debug for AtomicU32 {
//     fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
//         self.deref().fmt(fmt)
//     }
// }

pub(crate) struct AtomicUsize {
    inner: UnsafeCell<std::sync::atomic::AtomicUsize>,
}

unsafe impl Send for AtomicUsize {}
unsafe impl Sync for AtomicUsize {}

impl AtomicUsize {
    pub(crate) const fn new(val: usize) -> AtomicUsize {
        let inner = UnsafeCell::new(std::sync::atomic::AtomicUsize::new(val));
        AtomicUsize { inner }
    }

    /// Performs an unsynchronized load.
    ///
    /// # Safety
    ///
    /// All mutations must have happened before the unsynchronized load.
    /// Additionally, there must be no concurrent mutations.
    pub(crate) unsafe fn unsync_load(&self) -> usize {
        *(*self.inner.get()).get_mut()
    }
}

impl Deref for AtomicUsize {
    type Target = std::sync::atomic::AtomicUsize;

    fn deref(&self) -> &Self::Target {
        // safety: it is always safe to access `&self` fns on the inner value as
        // we never perform unsafe mutations.
        unsafe { &*self.inner.get() }
    }
}

impl fmt::Debug for AtomicUsize {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(fmt)
    }
}

pub(crate) struct AtomicPtr<T> {
    inner: UnsafeCell<std::sync::atomic::AtomicPtr<T>>,
}

unsafe impl<T> Send for AtomicPtr<T> {}
unsafe impl<T> Sync for AtomicPtr<T> {}

impl<T> AtomicPtr<T> {
    pub(crate) const fn new(val: *mut T) -> AtomicPtr<T> {
        let inner = UnsafeCell::new(std::sync::atomic::AtomicPtr::new(val));
        AtomicPtr { inner }
    }

    /// Performs an unsynchronized load.
    ///
    /// # Safety
    ///
    /// All mutations must have happened before the unsynchronized load.
    /// Additionally, there must be no concurrent mutations.
    pub(crate) unsafe fn unsync_load(&self) -> *mut T {
        *(*self.inner.get()).get_mut()
    }
}

impl<T> Deref for AtomicPtr<T> {
    type Target = std::sync::atomic::AtomicPtr<T>;

    fn deref(&self) -> &Self::Target {
        // safety: it is always safe to access `&self` fns on the inner value as
        // we never perform unsafe mutations.
        unsafe { &*self.inner.get() }
    }
}

impl<T> fmt::Debug for AtomicPtr<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(fmt)
    }
}
