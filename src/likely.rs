#[cfg(nightly)]
pub use std::intrinsics::{likely, unlikely};

#[cfg(not(nightly))]
#[inline]
#[cold]
const fn cold() {}

#[cfg(not(nightly))]
#[inline]
pub const fn likely(b: bool) -> bool {
    if !b {
        cold()
    }
    b
}

#[cfg(not(nightly))]
#[inline]
pub const fn unlikely(b: bool) -> bool {
    if b {
        cold()
    }
    b
}
